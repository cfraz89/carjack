package io.github.cfraz89.slipstream

import android.Manifest
import android.app.ActivityTaskManager
import android.app.PendingIntent
import android.app.Service
import android.bluetooth.BluetoothAdapter
import android.bluetooth.BluetoothDevice
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.SharedPreferences
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.graphics.drawable.Drawable
import android.hardware.display.DisplayManager
import android.os.Process
import android.os.RemoteException
import android.os.SystemClock
import android.os.SystemProperties
import android.os.UserHandle
import android.util.Log
import android.view.Display
import android.view.InputDevice
import android.view.InputEvent
import android.view.KeyCharacterMap
import android.view.KeyEvent
import android.view.Surface
import android.view.WindowManagerGlobal
import java.time.Duration

/**
 * The platform build: platform-signed and preinstalled, on a board with nobody at the screen, so
 * everything a user would be asked for is done directly.
 */
object Variant {
    private const val TAG = "carjack"

    /** Runtime permissions granted to ourselves, since nobody is there to answer a prompt. */
    private val RUNTIME_PERMISSIONS = arrayOf(
        Manifest.permission.BLUETOOTH_CONNECT,
        Manifest.permission.BLUETOOTH_SCAN,
        Manifest.permission.BLUETOOTH_ADVERTISE,
        Manifest.permission.ACCESS_FINE_LOCATION,
        Manifest.permission.ACCESS_COARSE_LOCATION,
        Manifest.permission.NEARBY_WIFI_DEVICES,
        Manifest.permission.POST_NOTIFICATIONS,
    )

    fun prepare(context: Context) {
        val pm = context.packageManager
        for (permission in RUNTIME_PERMISSIONS) {
            if (context.checkSelfPermission(permission) != PackageManager.PERMISSION_GRANTED) {
                try {
                    pm.grantRuntimePermission(
                        context.packageName, permission,
                        UserHandle.getUserHandleForUid(Process.myUid()),
                    )
                } catch (e: RuntimeException) {
                    Log.w(TAG, "Could not grant $permission", e)
                }
            }
        }
    }

    /** The platform build carries no resources of its own. */
    val NOTIFICATION_ICON = android.R.drawable.stat_sys_data_bluetooth

    /** No crash reporting on the board: logcat has it all. */
    val REPORTER: Native.Reporter? = null

    fun foregroundServiceTypes() = ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE

    /** Started from boot, and restarted if it dies. */
    fun onStartCommand(service: Service, intent: Intent?) = Service.START_STICKY

    /** The platform app reads sysfs itself. */
    fun interfaceMac(iface: String): String? = null

    /** The board keeps its own STA+STA local-only join. */
    fun joinPrimary(ssid: String, passphrase: String?) = false

    fun leavePrimary(ssid: String?) {}

    /** No app screen to show these on. */
    fun onStatus(context: Context, text: String, retryable: Boolean) {}

    fun onLinked(context: Context, linked: Boolean) {}

    /** Stopped until the next boot. */
    fun onStopRequested(context: Context) {}

    fun openApp(context: Context): PendingIntent? = PendingIntent.getActivity(
        context, 0, Intent(context, SettingsActivity::class.java), PendingIntent.FLAG_IMMUTABLE,
    )

    fun onServiceDestroyed(service: Service) {}

    const val PREFS = "carjack"

    /** As chosen in [SettingsActivity], else `persist.carjack.<key>`, which the product may set. */
    fun setting(context: Context, key: String, fallback: String): String =
        prefs(context).getString(key, null) ?: SystemProperties.get("persist.carjack.$key", fallback)

    fun prefs(context: Context): SharedPreferences =
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    /** What the head unit can open on connect besides an app, the first by default. */
    val ON_CONNECT = listOf(Previews.HOME)

    /** No built-in launcher on the board. */
    fun launcherIcon(context: Context): Drawable? = null

    fun makeDiscoverable(adapter: BluetoothAdapter) {
        // Zero is "never times out".
        adapter.setDiscoverableTimeout(Duration.ZERO)
        adapter.setScanMode(BluetoothAdapter.SCAN_MODE_CONNECTABLE_DISCOVERABLE)
    }

    /** Answers the pairing itself; true means the system's dialog is not needed. */
    fun confirmPairing(device: BluetoothDevice, intent: Intent): Boolean {
        val variant = intent.getIntExtra(BluetoothDevice.EXTRA_PAIRING_VARIANT, -1)
        Log.i(TAG, "Pairing request from ${device.address}, variant $variant")
        if (variant == BluetoothDevice.PAIRING_VARIANT_PIN) {
            device.setPin("0000")
        } else {
            device.setPairingConfirmation(true)
        }
        return true
    }

    fun canPage() = true

    fun isConnected(device: BluetoothDevice) = device.isConnected

    /** Page the head unit; false where this build can't. */
    fun connect(device: BluetoothDevice): Boolean = try {
        device.connect()
        true
    } catch (e: RuntimeException) {
        Log.w(TAG, "Could not page ${device.address}", e)
        false
    }

    fun disconnect(device: BluetoothDevice) {
        try {
            device.disconnect()
        } catch (e: RuntimeException) {
            Log.w(TAG, "Could not disconnect ${device.address}", e)
        }
    }

    /** Never needed: this build pages the head unit. */
    fun cycleBluetooth() = false

    fun bluetoothAddress(context: Context, adapter: BluetoothAdapter): String? = adapter.address

    fun blocker(context: Context): String? = null

    /** How long the screen is left as a dropped session had it, rather than opened afresh. */
    private const val KEEP_MS = 5 * 60_000L

    /** When the last session's display closed, by elapsedRealtime; 0 for none yet. */
    private var closedAt = 0L

    /**
     * The display the stream shows: the default one, resized to the part of the panel it gets and
     * mirrored 1:1 into [target], with what the head unit opens on connect.
     */
    fun openDisplay(context: Context, width: Int, height: Int, density: Int, target: Surface): Runnable {
        try {
            val wm = WindowManagerGlobal.getWindowManagerService()
            wm?.setForcedDisplaySize(Display.DEFAULT_DISPLAY, width, height)
            wm?.setForcedDisplayDensityForUser(Display.DEFAULT_DISPLAY, density, UserHandle.USER_CURRENT)
        } catch (e: Exception) {
            Log.w(TAG, "Could not resize the display", e)
        }
        val display = context.getSystemService(DisplayManager::class.java).createVirtualDisplay(
            "carjack", width, height, density, target,
            DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR or DisplayManager.VIRTUAL_DISPLAY_FLAG_SECURE,
        )
        Switchgear.watch(context)
        if (closedAt == 0L || SystemClock.elapsedRealtime() - closedAt > KEEP_MS) openOnConnect(context)
        return Runnable {
            Switchgear.stop()
            display.release()
            closedAt = SystemClock.elapsedRealtime()
        }
    }

    /** The home screen, or the chosen app while it's installed. */
    private fun openOnConnect(context: Context) {
        val app = Choices.current(context)
        val intent = if (Choices.isApp(app)) {
            ComponentName.unflattenFromString(app)?.let { Intent(Intent.ACTION_MAIN).setComponent(it) }
                ?: context.packageManager.getLaunchIntentForPackage(app)
        } else {
            null
        }
        if (intent == null) {
            if (Choices.isApp(app)) Log.w(TAG, "$app isn't installed; the head unit gets the home screen")
            goHome(context)
            return
        }
        try {
            context.startActivity(intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
        } catch (e: RuntimeException) {
            Log.w(TAG, "Could not open $app", e)
        }
    }

    /** Touches in the view rectangle land 1:1 on the resized default display. */
    fun touch(context: Context, viewX: Int, viewY: Int, viewWidth: Int, viewHeight: Int): Touch =
        TouchInjector(viewX, viewY, viewWidth, viewHeight)

    /** A key from the head unit's switchgear, through its mode; see [Switchgear]. */
    fun switchgear(context: Context, code: Int, down: Boolean) {
        Switchgear.key(context, code, down)
    }

    /** The app in front on the board's display, for [Switchgear]. */
    fun topPackage(): String? = try {
        ActivityTaskManager.getService().focusedRootTaskInfo?.topActivity?.packageName
    } catch (e: RemoteException) {
        null
    } catch (e: RuntimeException) {
        Log.w(TAG, "No top app", e)
        null
    }

    /** A key onto the board's own display, which is what the head unit mirrors; Home included. */
    fun key(context: Context, code: Int, down: Boolean) {
        inject(code, down, 0)
    }

    /** A key going down or up, with KeyEvent meta state. */
    fun inject(code: Int, down: Boolean, meta: Int) {
        val now = SystemClock.uptimeMillis()
        val event = KeyEvent(
            now, now, if (down) KeyEvent.ACTION_DOWN else KeyEvent.ACTION_UP, code, 0, meta,
            KeyCharacterMap.VIRTUAL_KEYBOARD, 0, 0, InputDevice.SOURCE_KEYBOARD,
        )
        try {
            val global = Class.forName("android.hardware.input.InputManagerGlobal")
            val input = global.getMethod("getInstance").invoke(null)
            global.getMethod("injectInputEvent", InputEvent::class.java, Int::class.javaPrimitiveType)
                .invoke(input, event, 0 /* INJECT_INPUT_EVENT_MODE_ASYNC */)
        } catch (e: ReflectiveOperationException) {
            Log.w(TAG, "Could not inject key $code", e)
        }
    }

    fun key(context: Context, code: Int) {
        key(context, code, true)
        key(context, code, false)
    }

    fun goHome(context: Context) {
        key(context, 3 /* KEYCODE_HOME */)
    }
}
