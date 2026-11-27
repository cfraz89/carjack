package io.github.cfraz89.slipstream

import android.app.ActivityManager
import android.app.PendingIntent
import android.app.Service
import android.bluetooth.BluetoothAdapter
import android.bluetooth.BluetoothDevice
import android.content.Context
import android.content.Intent
import android.content.SharedPreferences
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.graphics.drawable.Drawable
import android.os.Handler
import android.os.Looper
import android.os.Process
import android.os.RemoteException
import android.util.Log
import android.view.KeyEvent
import android.view.Surface
import io.github.cfraz89.slipstream.aa.AndroidAuto
import java.io.File
import java.io.IOException

/**
 * The generic build: an ordinary app on someone's phone, with Shizuku standing in for the
 * privileges the platform build has - the head unit's display, touch on it, Bluetooth cycling and the
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
        Profiles.global(context).edit().putBoolean("linked", false).apply()
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

    /** Restarted: it listens until turned off ([SlipstreamMode]). */
    fun onStartCommand(service: Service, intent: Intent?) = Service.START_STICKY

    /** For the tile and the phone remote, with Retry where it applies. */
    fun onStatus(context: Context, text: String, retryable: Boolean) {
        Profiles.global(context).edit().putString("status", text).putBoolean("retryable", retryable).apply()
        PhoneRemoteActivity.status = text
        PhoneRemoteActivity.retryable = retryable
    }

    /** Whether a unit's link is up, for [SettingsActivity]'s Bluetooth shortcut. */
    fun onLinked(context: Context, linked: Boolean) {
        Profiles.global(context).edit().putBoolean("linked", linked).apply()
    }

    /** The phone's screen during a session. */
    private const val PHONE_REMOTE = "$APP_ID/$APP_ID.PhoneRemoteActivity"

    /** The notification's Turn off stops the listening. */
    fun onStopRequested(context: Context) {
        SlipstreamMode.setListening(context, false)
    }

    fun openApp(context: Context): PendingIntent? = PendingIntent.getActivity(
        context, 0, Intent(context, PhoneRemoteActivity::class.java), PendingIntent.FLAG_IMMUTABLE,
    )

    /** How long the service's own teardown gets before the process goes. */
    private const val STOP_GRACE_MS = 1500L

    /**
     * Stop means stopped: the phone remote closes, and once the teardown has had its
     * moment the process ends, taking the native session, the listeners and any network request
     * with it - unless one of its screens is up, which would look like a crash. Shizuku goes only
     * once the WiFi is handed back.
     */
    fun onServiceDestroyed(service: Service) {
        PhoneRemoteActivity.close()
        AccessoryWifi.afterShell {
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

    /** The selected profile's settings. */
    fun prefs(context: Context): SharedPreferences = Profiles.prefs(context)

    /** Asked for from [SettingsActivity], for as long as Android allows. */
    fun makeDiscoverable(adapter: BluetoothAdapter) {}

    /** The user answers the system's dialog. */
    fun confirmPairing(device: BluetoothDevice, intent: Intent): Boolean {
        Log.i(TAG, "Pairing request from ${device.address}; the user confirms it")
        return false
    }

    /** No paging for apps: Slipstream dials the head unit's iAP service instead. */
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
     * units (the CFMOTO) need in CarPlayAvailability. Kept, so Android Auto can do without Shizuku.
     */
    fun bluetoothAddress(context: Context, adapter: BluetoothAdapter): String? {
        rememberBluetoothAddress(context)?.let { return it }
        Log.w(TAG, "No Bluetooth address from Shizuku; the head unit gets a placeholder")
        return adapter.address
    }

    /** The real Bluetooth address, read through Shizuku where it's up, else as last read. Blocks. */
    fun rememberBluetoothAddress(context: Context): String? {
        val read = ShizukuDisplay.bluetoothAddress()
        if (read != null) {
            Profiles.global(context).edit().putString("bluetooth_address", read).apply()
            return read
        }
        return Profiles.global(context).getString("bluetooth_address", null)
    }

    fun knowsBluetoothAddress(context: Context) = Profiles.global(context).contains("bluetooth_address")

    /**
     * What keeps a session from starting, or null: without Shizuku Slipstream mode has no display, and
     * units that check the Bluetooth address (the CFMOTO) never answer until it's been read once.
     */
    fun blocker(context: Context): String? {
        val problem = ShizukuDisplay.problem() ?: return null
        return when {
            SlipstreamMode.isOn(context) -> problem
            knowsBluetoothAddress(context) -> null
            else -> "$problem, and Slipstream hasn't read the phone's Bluetooth address yet"
        }
    }

    /**
     * The MAC [iface] joined with, which apps can't read and the head unit may check against the
     * station on its AP; null without Shizuku.
     */
    fun interfaceMac(iface: String) = ShizukuDisplay.interfaceMac(iface)

    /**
     * The phone's WiFi switched over to the head unit's AP, through Shizuku: one channel for the radio
     * rather than two time-sliced. False where it can't, for the local-only request instead.
     * Blocks.
     */
    fun joinPrimary(ssid: String, passphrase: String?) = ShizukuDisplay.joinPrimary(ssid, passphrase)

    /** Back off the head unit's AP; with a null SSID, just unlocks switching. Blocks. */
    fun leavePrimary(ssid: String?) {
        ShizukuDisplay.leavePrimary(ssid)
    }

    /** The built-in launcher's home screen, which the head unit gets unless another app is chosen. */
    private const val LAUNCHER = "$APP_ID/$APP_ID.launcher.HomeActivity"

    /** What the head unit can open on connect besides an app, the first by default. */
    /**
     * Not the phone's home screen: Pixel Launcher's home crashes on the head unit's display and its
     * second-screen home draws nothing there.
     */
    val ON_CONNECT = listOf(Choices.LAUNCHER)

    fun launcherIcon(context: Context): Drawable? = context.getDrawable(R.drawable.ic_launcher)

    /** The chosen app while it's still installed, else the launcher. */
    private fun appFor(context: Context): String {
        val app = Choices.current(context)
        if (app.isEmpty()) return LAUNCHER
        return try {
            context.packageManager.getPackageInfo(app.substringBefore('/'), 0)
            app
        } catch (e: PackageManager.NameNotFoundException) {
            Log.w(TAG, "$app isn't installed; the head unit gets the launcher")
            LAUNCHER
        }
    }

    /**
     * In Slipstream mode a display of the head unit's own size with the chosen app on it, or null
     * without Shizuku; otherwise Android Auto.
     */
    fun openDisplay(context: Context, width: Int, height: Int, density: Int, target: Surface): Runnable? {
        if (!SlipstreamMode.isOn(context)) {
            val close = AndroidAuto.open(context, width, height, target)
            showRemote(context)
            return Runnable {
                close.run()
                hideRemote()
            }
        }
        val close = ShizukuDisplay.open(target, width, height, density, appFor(context))
        if (close == null) {
            Log.e(TAG, "No display: Shizuku isn't running or hasn't let Slipstream in")
            return null
        }
        showRemote(context)
        return Runnable {
            hideRemote()
            close.run()
        }
    }

    /** How long the phone remote waits, once the display's gone, for a mode change to bring one back. */
    private const val REMOTE_LINGER_MS = 500L

    private fun showRemote(context: Context) {
        PhoneRemoteActivity.connected = true
        if (ShizukuDisplay.available()) {
            ShizukuDisplay.launchOnPhone(PHONE_REMOTE)
        } else {
            // Without Shizuku Android may refuse a start from the background; the notification opens it.
            try {
                context.startActivity(
                    Intent(context, PhoneRemoteActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
                )
            } catch (e: RuntimeException) {
                Log.w(TAG, "Couldn't open the phone remote", e)
            }
        }
    }

    private fun hideRemote() {
        PhoneRemoteActivity.connected = false
        Handler(Looper.getMainLooper()).postDelayed(
            { if (!PhoneRemoteActivity.connected) PhoneRemoteActivity.close() }, REMOTE_LINGER_MS,
        )
    }

    /** Slipstream mode's display with the chosen app, for [PreviewActivity]; null without Shizuku. */
    fun openPreview(context: Context, width: Int, height: Int, density: Int, target: Surface): Runnable? =
        ShizukuDisplay.openPreview(target, width, height, density, appFor(context))

    /** Onto the display, 1:1, or to Android Auto, whichever is showing. */
    fun touch(context: Context, viewX: Int, viewY: Int, viewWidth: Int, viewHeight: Int) =
        Touch { action, pointer, panelX, panelY ->
            if (AndroidAuto.active) {
                AndroidAuto.touch(action, pointer, panelX, panelY, viewX, viewY, viewWidth, viewHeight)
                return@Touch
            }
            val display = ShizukuDisplay.active ?: return@Touch
            try {
                display.touch(action, pointer, panelX - viewX, panelY - viewY)
            } catch (e: RemoteException) {
                Log.w(TAG, "Touch lost", e)
            }
        }

    /** The app showing on the head unit's display, for [Switchgear]. */
    fun topPackage() = ShizukuDisplay.topPackage()

    private const val KEYCODE_HOME = 3

    /** A key from the head unit's switchgear, through its mode ([Switchgear]), or to Android Auto. */
    fun switchgear(context: Context, code: Int, down: Boolean) {
        if (AndroidAuto.active) AndroidAuto.key(context, code, down) else Switchgear.key(context, code, down)
    }

    /** Whether the head unit gets Android's own display, which the system bar is for: Slipstream mode. */
    fun showsAndroid(context: Context) = SlipstreamMode.isOn(context)

    /** The temperature outside, for the system bar, as the launcher's weather has it. Blocks. */
    fun temperature(context: Context): String? =
        io.github.cfraz89.slipstream.launcher.Weather.now(context)?.let { "${it.temperature}${it.unit}" }

    /** Back or home from the phone remote's own buttons, where the switchgear's mapping doesn't apply. */
    fun navigate(context: Context, code: Int, down: Boolean) {
        if (AndroidAuto.active) AndroidAuto.navigate(code, down) else Switchgear.navigate(context, code, down)
    }

    /**
     * A key onto the display. Home isn't sent as a key: on a display without system decorations
     * that's the phone's own home; it reopens what the head unit opens on connect instead.
     */
    fun key(context: Context, code: Int, down: Boolean) {
        when (code) {
            KEYCODE_HOME -> if (down) goHome(context)
            KeyEvent.KEYCODE_VOICE_ASSIST -> if (down) voice()
            else -> inject(code, down, 0)
        }
    }

    fun inject(code: Int, down: Boolean, meta: Int) {
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

    /** Whether the head unit opens the built-in launcher on connect. */
    fun opensLauncher(context: Context) = appFor(context) == LAUNCHER

    /** The built-in launcher on the head unit, whatever it opens on connect. */
    fun openLauncher() {
        ShizukuDisplay.launch(LAUNCHER)
    }

    /** What the head unit opens on connect, brought back to the front. */
    fun goHome(context: Context) {
        val display = ShizukuDisplay.active ?: return
        try {
            display.launch(appFor(context))
        } catch (e: RemoteException) {
            Log.w(TAG, "Home lost", e)
        }
    }
}
