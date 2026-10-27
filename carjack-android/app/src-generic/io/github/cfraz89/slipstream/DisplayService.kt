package io.github.cfraz89.slipstream

import android.app.WallpaperManager
import android.content.ComponentName
import android.content.Context
import android.content.ContextWrapper
import android.graphics.Point
import android.graphics.Rect
import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.os.IBinder
import android.os.ParcelFileDescriptor
import android.os.SystemClock
import android.util.Log
import android.view.InputDevice
import android.view.InputEvent
import android.view.KeyCharacterMap
import android.view.KeyEvent
import android.view.Surface
import java.io.File
import java.io.FileNotFoundException
import java.io.IOException

/**
 * Runs in a process Shizuku starts as the shell user, which may create trusted displays and inject
 * input - what lets Slipstream give the head unit a display of its own resolution with apps running on
 * it, rather than a mirror of the phone. Hidden APIs are reached by reflection; that is allowed
 * here. Shizuku constructs it with a [Context], or with none.
 */
class DisplayService(private val context: Context?) : IDisplayService.Stub() {
    constructor() : this(null)

    private companion object {
        const val TAG = "carjack"
        const val SHELL = "com.android.shell"

        // Private (no PUBLIC) and in the default group (no OWN_DISPLAY_GROUP, which this Android
        // ignores anyway): together that makes it never-blank, rendering while the phone sleeps.
        // No SHOULD_SHOW_SYSTEM_DECORATIONS either; the launcher brings its own taskbar.
        const val FLAGS = DisplayManager.VIRTUAL_DISPLAY_FLAG_OWN_CONTENT_ONLY or
            (1 shl 6) or // SUPPORTS_TOUCH
            (1 shl 8) or // DESTROY_CONTENT_ON_REMOVAL
            (1 shl 10) or // TRUSTED
            (1 shl 14) // OWN_FOCUS: its own input focus, apart from the phone's

        /** Where the system log goes: Downloads, so it can be sent on without adb or a shell app. */
        const val SYSTEM_LOG = "/sdcard/Download/slipstream-system.log"
        val SYSTEM_LOG_SUFFIXES = listOf("", ".1", ".2", ".3")

        /** What logs info worth keeping: Slipstream's own lines, and the parts a session leans on. */
        val SYSTEM_LOG_TAGS = listOf(
            "carjack", "AndroidRuntime", "ActivityManager", "ActivityTaskManager", "WindowManager",
            "PowerManagerService", "DisplayManagerService", "DisplayDeviceRepository",
            "KeyguardViewMediator", "WifiClientModeImpl", "WifiNetworkFactory",
            "WifiConnectivityManager", "WifiNative", "ConnectivityService", "BluetoothAdapter",
            "BluetoothManagerService", "AdapterService", "UserServiceManager", "MediaCodec",
            "CCodec",
        )

        const val PRIMARY_JOIN_MS = 20_000L
        const val PRIMARY_POLL_MS = 500L
        val MAC = Regex("MAC: ([0-9a-fA-F:]{17})")

        /**
         * The keyboard on this display, for what's typed into here: by default Android shows it
         * on the phone's (DISPLAY_IME_POLICY_FALLBACK_DISPLAY). Needs INTERNAL_SYSTEM_WINDOW,
         * which the shell has, and a trusted display, which this is.
         */
        fun keyboardHere(displayId: Int) {
            try {
                val binder = Class.forName("android.os.ServiceManager")
                    .getMethod("getService", String::class.java).invoke(null, "window") as IBinder
                val windows = Class.forName("android.view.IWindowManager\$Stub")
                    .getMethod("asInterface", IBinder::class.java).invoke(null, binder)
                windows.javaClass
                    .getMethod("setDisplayImePolicy", Int::class.javaPrimitiveType, Int::class.javaPrimitiveType)
                    .invoke(windows, displayId, 0 /* DISPLAY_IME_POLICY_LOCAL */)
            } catch (e: ReflectiveOperationException) {
                Log.w(TAG, "The keyboard stays on the phone", e)
            } catch (e: RuntimeException) {
                Log.w(TAG, "The keyboard stays on the phone", e)
            }
        }

        /** Onto [displayId], asynchronously, through the hidden InputManagerGlobal. */
        fun inject(event: InputEvent, displayId: Int) {
            InputEvent::class.java.getMethod("setDisplayId", Int::class.javaPrimitiveType)
                .invoke(event, displayId)
            val global = Class.forName("android.hardware.input.InputManagerGlobal")
            val input = global.getMethod("getInstance").invoke(null)
            global.getMethod("injectInputEvent", InputEvent::class.java, Int::class.javaPrimitiveType)
                .invoke(input, event, 0 /* INJECT_INPUT_EVENT_MODE_ASYNC */)
        }

        fun run(vararg command: String): String {
            try {
                val process = ProcessBuilder(*command).redirectErrorStream(true).start()
                val out = String(process.inputStream.readBytes())
                process.waitFor()
                return out
            } catch (e: IOException) {
                // Not the arguments: a WiFi join's include the password.
                Log.w(TAG, "Could not run ${command[0]}", e)
            } catch (e: InterruptedException) {
                Thread.currentThread().interrupt()
            }
            return ""
        }
    }

    private var display: VirtualDisplay? = null
    /** The phone kept awake while the display is up. */
    private var awake: StayAwake? = null
    /** The phone's screen dark but the phone awake, so the head unit's touches still land. */
    private var panel: PanelKeeper? = null
    private var fingers = Fingers()
    private var systemLog: Process? = null

    init {
        startSystemLog()
        PanelKeeper.restoreLeftovers()
    }

    private val displayId get() = display?.display?.displayId

    /** The system log's files, for the app to send on. */
    override fun systemLog(part: Int): ParcelFileDescriptor? {
        val suffix = SYSTEM_LOG_SUFFIXES.getOrNull(part) ?: return null
        return try {
            ParcelFileDescriptor.open(File(SYSTEM_LOG + suffix), ParcelFileDescriptor.MODE_READ_ONLY)
        } catch (e: FileNotFoundException) {
            null
        }
    }

    /**
     * The phone's own log - WiFi, connectivity, Bluetooth, power, displays - which the app's log
     * can't see, while Slipstream is running: warnings from everything, the rest only from what
     * bears on a session, so days of it fit. Four 8 MB files.
     */
    private fun startSystemLog() {
        // The last service's logcat outlives it, and two writers would rotate the files over each
        // other: it goes first. -T keeps the new one from replaying what the last one wrote.
        run("pkill", "-f", SYSTEM_LOG)
        try {
            val command = listOf(
                "logcat", "-b", "main,system,crash", "-v", "time", "-T", "1",
                "-f", SYSTEM_LOG, "-r", "8192", "-n", "3", "*:W",
            ) + SYSTEM_LOG_TAGS.map { "$it:I" }
            systemLog = ProcessBuilder(command).start()
        } catch (e: IOException) {
            Log.w(TAG, "No system log", e)
        }
    }

    /** The shell's package: the display service checks the caller's package against its uid. */
    private fun shellContext(): Context = object : ContextWrapper(context) {
        override fun getPackageName() = SHELL

        override fun getOpPackageName() = SHELL
    }

    @Synchronized
    override fun create(surface: Surface, width: Int, height: Int, density: Int): Int {
        release()
        return try {
            val dm = DisplayManager::class.java.getDeclaredConstructor(Context::class.java)
                .newInstance(shellContext())
            val created = dm.createVirtualDisplay("slipstream", width, height, density, surface, FLAGS)
            display = created
            fingers = Fingers()
            val id = created.display.displayId
            Log.i(TAG, "Display $id at ${width}x$height, ${density}dpi")
            awake = StayAwake(shellContext()).also { it.start() }
            keyboardHere(id)
            id
        } catch (e: ReflectiveOperationException) {
            Log.e(TAG, "Could not create a display", e)
            -1
        } catch (e: RuntimeException) {
            Log.e(TAG, "Could not create a display", e)
            -1
        }
    }

    @Synchronized
    override fun touch(action: Int, pointer: Int, x: Float, y: Float) {
        val id = displayId ?: return
        val event = fingers.event(action, pointer, x, y) ?: return
        try {
            inject(event, id)
        } catch (e: ReflectiveOperationException) {
            Log.w(TAG, "Could not inject a touch", e)
        } finally {
            event.recycle()
        }
    }

    override fun allowNotificationListener(component: String) {
        run("cmd", "notification", "allow_listener", component)
    }

    /** From the trust service's dump: the current user's line reads trustState=TRUSTED. */
    override fun trusted(): Boolean {
        val line = run("dumpsys", "trust").lineSequence()
            .firstOrNull { "(current)" in it && "trustState=" in it } ?: return false
        return "trustState=TRUSTED" in line
    }

    /** A key going down or up on the display, for its focused window (or, media keys, the player). */
    @Synchronized
    override fun key(code: Int, down: Boolean, meta: Int) {
        val id = displayId ?: return
        val now = SystemClock.uptimeMillis()
        val event = KeyEvent(
            now, now, if (down) KeyEvent.ACTION_DOWN else KeyEvent.ACTION_UP, code, 0, meta,
            KeyCharacterMap.VIRTUAL_KEYBOARD, 0, 0, InputDevice.SOURCE_KEYBOARD,
        )
        try {
            inject(event, id)
        } catch (e: ReflectiveOperationException) {
            Log.w(TAG, "Could not inject key $code", e)
        }
    }

    @Synchronized
    override fun launch(component: String) {
        val id = displayId ?: return
        var target = component
        if ("/" !in target) {
            // A package: its launcher activity, as the last line resolve-activity prints.
            val resolved = run(
                "cmd", "package", "resolve-activity", "--brief", "-c",
                "android.intent.category.LAUNCHER", target,
            ).trim()
            target = resolved.substringAfterLast('\n')
            if ("/" !in target) {
                Log.w(TAG, "No launcher activity in $resolved")
                return
            }
        }
        run("am", "start", "--display", id.toString(), "-n", target)
    }

    /** Hands-free voice search, an activity, where the assistant key's overlay is the phone's. */
    @Synchronized
    override fun voice() {
        val id = displayId ?: return
        run(
            "am", "start", "--display", id.toString(),
            "-a", "android.intent.action.VOICE_COMMAND", "-f", "0x10000000",
        )
    }

    /** From the activity manager's root tasks on the display, topmost first: the visible one's app. */
    override fun topPackage(): String? {
        val id = displayId ?: return null
        return try {
            val atm = Class.forName("android.app.ActivityTaskManager").getMethod("getService").invoke(null)
            val tasks = atm.javaClass
                .getMethod("getAllRootTaskInfosOnDisplay", Int::class.javaPrimitiveType)
                .invoke(atm, id) as List<*>
            val visible = tasks.filterNotNull().filter {
                try {
                    it.javaClass.getField("isVisible").getBoolean(it)
                } catch (e: NoSuchFieldException) {
                    true
                }
            }
            visible.firstNotNullOfOrNull {
                (it.javaClass.getField("topActivity").get(it) as? ComponentName)?.packageName
            }
        } catch (e: ReflectiveOperationException) {
            Log.w(TAG, "No top app for display $id", e)
            null
        } catch (e: RuntimeException) {
            Log.w(TAG, "No top app for display $id", e)
            null
        }
    }

    /** Asleep, a lock screen can't be dismissed: the phone wakes first, a no-op when it's awake. */
    override fun dismissKeyguard() {
        run("input", "keyevent", "KEYCODE_WAKEUP")
        run("wm", "dismiss-keyguard")
    }

    override fun cycleBluetooth() {
        run("cmd", "bluetooth_manager", "disable")
        run("cmd", "bluetooth_manager", "wait-for-state:STATE_OFF")
        run("cmd", "bluetooth_manager", "enable")
        run("cmd", "bluetooth_manager", "wait-for-state:STATE_ON")
    }

    override fun bluetoothAddress(): String =
        run("settings", "get", "secure", "bluetooth_address").trim()

    override fun grant(pkg: String, permissions: Array<String>) {
        for (permission in permissions) run("pm", "grant", pkg, permission)
    }

    /**
     * From dumpsys wifi, which the shell may read where sysfs is closed to it: each client mode's
     * `mLinkProperties {InterfaceName: <iface> ...` is followed by its
     * `mWifiInfo ..., MAC: <the address it joined with>, ...`.
     */
    override fun interfaceMac(iface: String): String? {
        val prefix = "mLinkProperties {InterfaceName: "
        var inInterface: String? = null
        for (raw in run("dumpsys", "wifi").lineSequence()) {
            val line = raw.trim()
            if (line.startsWith(prefix)) {
                inInterface = line.removePrefix(prefix).substringBefore(' ')
            } else if (line.startsWith("mWifiInfo ") && iface == inInterface) {
                val mac = MAC.find(line)?.groupValues?.get(1)
                if (mac != null && mac != "02:00:00:00:00:00") return mac
            }
        }
        return null
    }

    override fun enableBluetooth() {
        run("cmd", "bluetooth_manager", "enable")
        run("cmd", "bluetooth_manager", "wait-for-state:STATE_ON")
    }

    /**
     * The phone's WiFi switched over to the head unit's AP, so the radio stays on its channel rather
     * than time-slicing with home WiFi on another. Switching to another saved network while
     * connected is locked out (the associated network selection override); autojoin stays off for
     * it, so it's never joined outside a session.
     */
    override fun joinPrimary(ssid: String, passphrase: String?): Boolean {
        run("cmd", "wifi", "set-network-selection-config", "enabled", "enabled", "-a", "2")
        if (passphrase.isNullOrEmpty()) {
            run("cmd", "wifi", "connect-network", ssid, "open", "-d")
        } else {
            run("cmd", "wifi", "connect-network", ssid, "wpa2", passphrase, "-d")
        }
        val connected = "Wifi is connected to \"$ssid\""
        val deadline = SystemClock.uptimeMillis() + PRIMARY_JOIN_MS
        while (SystemClock.uptimeMillis() < deadline) {
            if (connected in run("cmd", "wifi", "status")) return true
            SystemClock.sleep(PRIMARY_POLL_MS)
        }
        Log.w(TAG, "WiFi not switched over to $ssid after ${PRIMARY_JOIN_MS}ms")
        leavePrimary(ssid)
        return false
    }

    override fun leavePrimary(ssid: String?) {
        if (ssid != null) {
            // list-networks rows: "<id> <ssid> <security>".
            for (line in run("cmd", "wifi", "list-networks").lineSequence()) {
                val cols = line.trim().split(Regex("\\s+"))
                if (cols.size >= 3 && cols[0].all { it.isDigit() } && " $ssid " in line) {
                    run("cmd", "wifi", "forget-network", cols[0])
                }
            }
        }
        run("cmd", "wifi", "set-network-selection-config", "enabled", "enabled", "-a", "0")
    }

    // The wallpaper calls below are hidden, and need READ_WALLPAPER_INTERNAL, which the shell has:
    // they're how a home screen of the head unit's size would frame the phone's wallpaper, from the
    // original rather than the phone's own portrait crop of it.

    override fun wallpaperOriginal(): ParcelFileDescriptor? = try {
        WallpaperManager::class.java
            .getMethod("getWallpaperFile", Int::class.javaPrimitiveType, Boolean::class.javaPrimitiveType)
            .invoke(wallpapers(), WallpaperManager.FLAG_SYSTEM, false) as ParcelFileDescriptor?
    } catch (e: ReflectiveOperationException) {
        Log.w(TAG, "No original wallpaper", e)
        null
    } catch (e: RuntimeException) {
        Log.w(TAG, "No original wallpaper", e)
        null
    }

    override fun wallpaperCrop(width: Int, height: Int): IntArray? = try {
        @Suppress("UNCHECKED_CAST")
        val crops = WallpaperManager::class.java
            .getMethod(
                "getBitmapCrops", List::class.java, Int::class.javaPrimitiveType,
                Boolean::class.javaPrimitiveType,
            )
            .invoke(wallpapers(), listOf(Point(width, height)), WallpaperManager.FLAG_SYSTEM, true)
            as List<Rect>?
        crops?.firstOrNull()?.let { intArrayOf(it.left, it.top, it.right, it.bottom) }
    } catch (e: ReflectiveOperationException) {
        Log.w(TAG, "No wallpaper crop for ${width}x$height", e)
        null
    } catch (e: RuntimeException) {
        Log.w(TAG, "No wallpaper crop for ${width}x$height", e)
        null
    }

    override fun secureSetting(key: String): String? {
        val value = run("settings", "get", "secure", key).trim()
        return if (value.isEmpty() || value == "null") null else value
    }

    override fun setSecureSetting(key: String, value: String?) {
        if (value == null) {
            run("settings", "delete", "secure", key)
        } else {
            run("settings", "put", "secure", key, value)
        }
    }

    private fun wallpapers() = WallpaperManager.getInstance(shellContext())

    @Synchronized
    override fun keepPanel(keep: Boolean) {
        if (!keep || display == null) {
            panel?.stop()
            panel = null
            return
        }
        if (panel == null) {
            try {
                panel = PanelKeeper(shellContext()).also { it.start() }
            } catch (e: ReflectiveOperationException) {
                Log.w(TAG, "No panel keeping; the phone sleeps as usual", e)
                panel = null
            } catch (e: RuntimeException) {
                Log.w(TAG, "No panel keeping; the phone sleeps as usual", e)
                panel = null
            }
        }
    }

    @Synchronized
    override fun release() {
        panel?.stop()
        panel = null
        awake?.stop()
        awake = null
        display?.release()
        display = null
    }

    override fun destroy() {
        release()
        systemLog?.destroy()
        System.exit(0)
    }
}
