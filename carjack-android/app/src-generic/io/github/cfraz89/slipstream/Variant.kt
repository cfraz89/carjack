package io.github.cfraz89.slipstream

import android.app.ActivityManager
import android.app.KeyguardManager
import android.app.PendingIntent
import android.app.Service
import android.bluetooth.BluetoothAdapter
import android.bluetooth.BluetoothDevice
import android.content.BroadcastReceiver
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.SharedPreferences
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.os.Handler
import android.os.Looper
import android.os.Process
import android.os.RemoteException
import android.util.Log
import android.view.KeyEvent
import android.view.Surface
import java.io.File
import java.io.IOException

/**
 * The generic build: an ordinary app on someone's phone, with Shizuku standing in for the
 * privileges the platform build has - the unit's display, touch on it, Bluetooth cycling and the
 * phone's Bluetooth address. Permissions and pairing are asked of the user.
 */
object Variant {
    private const val TAG = "carjack"

    const val PREFS = "slipstream"
    val NOTIFICATION_ICON = R.drawable.ic_sl_notify
    const val LOG_FILE = "slipstream.log"
    val REPORTER: Native.Reporter? get() = Reports.CORE
    /** The installed app's ID, for Shizuku's service component, which is built statically. */
    const val APP_ID = "io.github.cfraz89.slipstream"

    private var logcat: java.lang.Process? = null

    /**
     * Our own log, kept in files/slipstream.log (and .1-.9) under Android/data, so a session away
     * from adb can be read afterwards - days later: info and up only, the core's chatter trimmed,
     * 40 MB in all. -T: from now, not the whole ring buffer again after each restart.
     */
    fun prepare(context: Context) {
        OffNotice.hide(context)
        // Not linked yet: puts back a lock delay a crash left held.
        prefs(context).edit().putBoolean("linked", false).apply()
        applyLockHold(context)
        val dir = context.getExternalFilesDir(null)
        if (dir == null || logcat != null) return
        try {
            logcat = ProcessBuilder(
                "logcat", "-v", "time", "-T", "1", "-f", File(dir, LOG_FILE).path,
                "-r", "4096", "-n", "9", "*:I",
            ).start()
        } catch (e: IOException) {
            Log.w(TAG, "No log file", e)
        }
    }

    fun foregroundServiceTypes() = ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE

    fun onStartCommand(service: Service, intent: Intent?) = Service.START_STICKY

    /** For [MainActivity] to show beside the switch, with Retry where it applies. */
    fun onStatus(context: Context, text: String, retryable: Boolean) {
        prefs(context).edit().putString("status", text).putBoolean("retryable", retryable).apply()
    }

    /** Whether a unit's link is up, for [MainActivity]'s Bluetooth shortcut. */
    fun onLinked(context: Context, linked: Boolean) {
        prefs(context).edit().putBoolean("linked", linked).apply()
        applyLockHold(context)
        watchScreenOff(context.applicationContext, linked)
        if (linked) checkTrustSoon(context.applicationContext)
    }

    /** Whether the power button has been seen locking the phone through the lock hold. */
    const val POWER_LOCKS = "power_locks"
    private const val LOCK_CHECK_DELAY_MS = 2000L
    private var screenOff: BroadcastReceiver? = null

    /**
     * With the lock hold on, a screen timeout can't lock the phone, so locking at screen-off is
     * the power button's "instantly locks"; the shell can't read that setting itself.
     */
    private fun watchScreenOff(app: Context, linked: Boolean) {
        if (linked && screenOff == null) {
            val receiver = object : BroadcastReceiver() {
                override fun onReceive(context: Context, intent: Intent) {
                    Handler(Looper.getMainLooper()).postDelayed({
                        if (!prefs(app).getBoolean("linked", false) || !holdsUnlock(app)) {
                            return@postDelayed
                        }
                        val locked = app.getSystemService(KeyguardManager::class.java).isDeviceLocked
                        Log.i(TAG, "Locked at screen-off while held: $locked")
                        prefs(app).edit().putBoolean(POWER_LOCKS, locked).apply()
                    }, LOCK_CHECK_DELAY_MS)
                }
            }
            screenOff = receiver
            app.registerReceiver(receiver, IntentFilter(Intent.ACTION_SCREEN_OFF))
        } else if (!linked) {
            screenOff?.let { app.unregisterReceiver(it) }
            screenOff = null
        }
    }

    /** Smart Lock takes a moment to trust a device that's just connected. */
    private const val TRUST_CHECK_DELAY_MS = 15_000L

    /**
     * Whether the unit keeps the phone unlocked (a trusted device), noted once seen so the
     * reminder to set it up can go. Seen only, never unset: trust also lapses for other reasons.
     */
    private fun checkTrustSoon(app: Context) {
        Handler(Looper.getMainLooper()).postDelayed({
            AccessoryWifi.afterShell {
                if (!prefs(app).getBoolean("linked", false)) return@afterShell
                val trusted = ShizukuDisplay.trusted()
                Log.i(TAG, "Phone trusted while the unit's connected: $trusted")
                if (trusted == true) prefs(app).edit().putBoolean("trust_seen", true).apply()
            }
        }, TRUST_CHECK_DELAY_MS)
    }

    private const val LOCK_AFTER = "lock_screen_lock_after_timeout"
    /** A day: in effect, no locking on screen-off while the unit is connected. */
    private val HELD_LOCK_AFTER = (24 * 60 * 60 * 1000).toString()

    /** Whether the phone's panel goes dark rather than the phone sleeping; off until proven. */
    fun keepsPanel(context: Context) = prefs(context).getBoolean("keep_panel", false)

    /** Whether to keep the phone unlocked while a unit's connected; the user's choice. */
    fun holdsUnlock(context: Context) = prefs(context).getBoolean("hold_unlock", true)

    /**
     * While a unit's connected (and the user wants it), the screen going off doesn't lock the
     * phone: the lock screen would hide the apps on the unit's display, which shares its display
     * group. Done as Smart Lock's trusted devices do, by holding off the lock after screen-off;
     * the power button still locks if it's set to lock instantly. Off the caller's thread.
     */
    fun applyLockHold(context: Context) {
        val app = context.applicationContext
        val hold = prefs(app).getBoolean("linked", false) && holdsUnlock(app) && BootReceiver.isOn(app)
        AccessoryWifi.afterShell { holdLock(app, hold) }
    }

    /** Swaps the lock delay for [HELD_LOCK_AFTER], keeping the user's to put back. */
    @Synchronized
    private fun holdLock(context: Context, hold: Boolean) {
        val prefs = prefs(context)
        if (hold == prefs.getBoolean("lock_held", false)) return
        if (hold) {
            // Its own instance, told apart from a real answer by identity.
            val unknown = String(charArrayOf('?'))
            val original = ShizukuDisplay.secureSetting(LOCK_AFTER, unknown)
            if (original === unknown || !ShizukuDisplay.setSecureSetting(LOCK_AFTER, HELD_LOCK_AFTER)) {
                return
            }
            // Unset is kept as absent, and put back as unset.
            prefs.edit().putBoolean("lock_held", true).putString("lock_after_saved", original).commit()
            Log.i(TAG, "Holding off the lock while the unit's connected (was $original)")
        } else {
            if (!ShizukuDisplay.setSecureSetting(LOCK_AFTER, prefs.getString("lock_after_saved", null))) {
                return
            }
            prefs.edit().putBoolean("lock_held", false).remove("lock_after_saved").commit()
            Log.i(TAG, "Lock delay put back")
        }
    }

    /** The notification's Turn off and Turn on flip the app's own switch. */
    fun onStopRequested(context: Context) {
        BootReceiver.setOn(context, false)
    }

    fun onStartRequested(context: Context) {
        BootReceiver.setOn(context, true)
    }

    fun openApp(context: Context): PendingIntent? = PendingIntent.getActivity(
        context, 0, Intent(context, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE,
    )

    /** How long the service's own teardown gets before the process goes. */
    private const val STOP_GRACE_MS = 1500L

    /**
     * Stop means stopped: once the teardown has had its moment the process ends, taking the
     * native session, the listeners and any network request with it - unless one of its screens
     * is up, which would look like a crash. Shizuku goes only once the WiFi is handed back.
     */
    fun onServiceDestroyed(service: Service) {
        val app = service.applicationContext
        OffNotice.show(app)
        AccessoryWifi.afterShell {
            holdLock(app, false)
            ShizukuDisplay.unbind()
            Handler(Looper.getMainLooper()).postDelayed({
                val state = ActivityManager.RunningAppProcessInfo()
                ActivityManager.getMyMemoryState(state)
                if (state.importance <= ActivityManager.RunningAppProcessInfo.IMPORTANCE_VISIBLE) {
                    return@postDelayed
                }
                logcat?.destroy()
                Process.killProcess(Process.myPid())
            }, STOP_GRACE_MS)
        }
    }

    fun setting(context: Context, key: String, fallback: String): String =
        prefs(context).getString(key, fallback) ?: fallback

    fun prefs(context: Context): SharedPreferences =
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    /** Asked for from [MainActivity], for as long as Android allows. */
    fun makeDiscoverable(adapter: BluetoothAdapter) {}

    /** The user answers the system's dialog. */
    fun confirmPairing(device: BluetoothDevice, intent: Intent): Boolean {
        Log.i(TAG, "Pairing request from ${device.address}; the user confirms it")
        return false
    }

    /** No paging for apps: Slipstream dials the unit's iAP service instead. */
    fun canPage() = false

    /**
     * Apps can neither see the link nor page a device, so it is taken as up and left to the
     * phone's own profiles, which reconnect a bonded unit.
     */
    fun isConnected(device: BluetoothDevice) = true

    /** Apps can't page a device; the phone's own profiles reconnect it. */
    fun connect(device: BluetoothDevice) = false

    fun disconnect(device: BluetoothDevice) {}

    /** Bluetooth off and on, for a fresh link. */
    fun cycleBluetooth() = ShizukuDisplay.cycleBluetooth()

    /**
     * Apps only see a placeholder for their own address; Shizuku reads the real one, which some
     * units (the CFMOTO) need in CarPlayAvailability.
     */
    fun bluetoothAddress(context: Context, adapter: BluetoothAdapter): String? {
        ShizukuDisplay.bluetoothAddress()?.let { return it }
        Log.w(TAG, "No Bluetooth address from Shizuku; the unit gets a placeholder")
        return adapter.address
    }

    /**
     * The MAC [iface] joined with, which apps can't read and the unit may check against the
     * station on its AP; null without Shizuku.
     */
    fun interfaceMac(iface: String) = ShizukuDisplay.interfaceMac(iface)

    /**
     * The phone's WiFi switched over to the unit's AP, through Shizuku: one channel for the radio
     * rather than two time-sliced. False where it can't, for the local-only request instead.
     * Blocks.
     */
    fun joinPrimary(ssid: String, passphrase: String?) = ShizukuDisplay.joinPrimary(ssid, passphrase)

    /** Back off the unit's AP; with a null SSID, just unlocks switching. Blocks. */
    fun leavePrimary(ssid: String?) {
        ShizukuDisplay.leavePrimary(ssid)
    }

    /** The built-in launcher's home screen, which the unit gets unless another app is chosen. */
    private const val LAUNCHER = "$APP_ID/$APP_ID.launcher.HomeActivity"

    /** The chosen app while it's still installed, else the launcher. */
    private fun appFor(context: Context): String {
        val app = setting(context, "app", "")
        if (app.isEmpty()) return LAUNCHER
        if (app == Previews.HOME) return homeApp(context)?.flattenToString() ?: LAUNCHER
        return try {
            context.packageManager.getPackageInfo(app.substringBefore('/'), 0)
            app
        } catch (e: PackageManager.NameNotFoundException) {
            Log.w(TAG, "$app isn't installed; the unit gets the launcher")
            LAUNCHER
        }
    }

    /** The phone's own home app, or null where none is set as the default. */
    fun homeApp(context: Context): ComponentName? {
        val info = context.packageManager.resolveActivity(
            Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_HOME),
            PackageManager.MATCH_DEFAULT_ONLY,
        )
        // With no default, the chooser resolves; it isn't a home app.
        val activity = info?.activityInfo ?: return null
        if (activity.packageName == "android") return null
        return ComponentName(activity.packageName, activity.name)
    }

    /** A display of the unit's own size with the chosen app on it, or null without Shizuku. */
    fun openDisplay(context: Context, width: Int, height: Int, density: Int, target: Surface): Runnable? {
        val close = ShizukuDisplay.open(target, width, height, density, appFor(context), keepsPanel(context))
        if (close == null) Log.e(TAG, "No display: Shizuku isn't running or hasn't let Slipstream in")
        return close
    }

    /** Onto the display, 1:1. */
    fun touch(context: Context, viewX: Int, viewY: Int, viewWidth: Int, viewHeight: Int) =
        Touch { action, pointer, panelX, panelY ->
            val display = ShizukuDisplay.active ?: return@Touch
            try {
                display.touch(action, pointer, panelX - viewX, panelY - viewY)
            } catch (e: RemoteException) {
                Log.w(TAG, "Touch lost", e)
            }
        }

    private const val KEYCODE_HOME = 3
    private const val SELECT_HOLD_MS = 150L

    /**
     * A key from the switchgear or the bar onto the display. Home isn't sent as a key: on a
     * display without system decorations that's the phone's own home; it reopens what the unit
     * opens on connect instead.
     */
    fun key(context: Context, code: Int, down: Boolean) {
        if (code == KEYCODE_HOME) {
            if (down) goHome(context)
            return
        }
        if (code == KeyEvent.KEYCODE_VOICE_ASSIST) {
            if (down) voice()
            return
        }
        // Up and down as Shift+Tab and Tab: focus then visits everything on the screen in reading
        // order, wrapping round, where the arrows only reach what lies above or below.
        when (code) {
            KeyEvent.KEYCODE_DPAD_DOWN -> inject(KeyEvent.KEYCODE_TAB, down, 0)
            KeyEvent.KEYCODE_DPAD_UP -> inject(
                KeyEvent.KEYCODE_TAB, down, KeyEvent.META_SHIFT_ON or KeyEvent.META_SHIFT_LEFT_ON,
            )
            // The unit lets go of select within a few ms; held a moment, the press shows.
            KeyEvent.KEYCODE_DPAD_CENTER -> if (down) {
                inject(code, true, 0)
            } else {
                Handler(Looper.getMainLooper()).postDelayed({ inject(code, false, 0) }, SELECT_HOLD_MS)
            }
            else -> inject(code, down, 0)
        }
    }

    private fun inject(code: Int, down: Boolean, meta: Int) {
        val display = ShizukuDisplay.active ?: return
        try {
            display.key(code, down, meta)
        } catch (e: RemoteException) {
            Log.w(TAG, "Key $code lost", e)
        }
    }

    private fun voice() {
        val display = ShizukuDisplay.active ?: return
        try {
            display.voice()
        } catch (e: RemoteException) {
            Log.w(TAG, "Voice lost", e)
        }
    }

    /** A press and release. */
    fun key(context: Context, code: Int) {
        key(context, code, true)
        key(context, code, false)
    }

    /** What the unit opens on connect, brought back to the front. */
    fun goHome(context: Context) {
        val display = ShizukuDisplay.active ?: return
        try {
            display.launch(appFor(context))
        } catch (e: RemoteException) {
            Log.w(TAG, "Home lost", e)
        }
    }
}
