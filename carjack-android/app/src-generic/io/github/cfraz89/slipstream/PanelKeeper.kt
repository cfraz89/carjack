package io.github.cfraz89.slipstream

import android.content.Context
import android.hardware.display.DisplayManager
import android.os.PowerManager
import android.os.SystemClock
import android.util.Log
import android.view.Display
import java.io.File
import java.io.FileInputStream
import java.io.IOException
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.concurrent.atomic.AtomicBoolean

/**
 * The phone's screen going dark without the phone going to sleep, while the unit's display is
 * up: asleep, Android drops the touches sent to the unit, whose display shares the phone's
 * display group, and a lock screen would hide its apps. So for the session Android's own timeout
 * is held off and the power button's short press does nothing; in their place the panel is
 * switched off (`cmd display power-off`) once the phone's touchscreen has been idle for the
 * user's timeout, or at a press of the power button. A touch or another press brings it back.
 * Runs in the shell.
 */
class PanelKeeper(shellContext: Context) {
    companion object {
        private const val TAG = "carjack"
        private const val SYSTEM = "system"
        private const val GLOBAL = "global"
        private const val TIMEOUT = "screen_off_timeout"
        private const val BRIGHTNESS_MODE = "screen_brightness_mode"
        private const val POWER_PRESS = "power_button_short_press"
        /** Never, in effect. */
        private val HELD_TIMEOUT = Int.MAX_VALUE.toString()
        /** PhoneWindowManager's SHORT_PRESS_POWER_NOTHING: the press is ours to act on. */
        private const val POWER_PRESS_NOTHING = "0"
        private const val DEFAULT_TIMEOUT_MS = 30_000L
        private const val TICK_MS = 1_000L
        /**
         * What to put back, kept on disk so a crash can't leave the phone never sleeping: the
         * timeout, the power press, and the brightness mode while the panel is dark. A line is
         * empty where the setting was unset.
         */
        private val SAVED = File("/data/local/tmp/slipstream-panel")

        private const val EV_KEY = 1
        private const val KEY_POWER = 116
        /** struct input_event on arm64: a 16-byte timeval, then u16 type, u16 code, s32 value. */
        private const val EVENT_SIZE = 24

        /** Puts back what a keeper that never got to stop left changed. */
        fun restoreLeftovers() {
            if (!SAVED.exists()) return
            try {
                val saved = SAVED.readText().split("\n")
                put(SYSTEM, TIMEOUT, saved[0])
                if (saved.size > 1) put(GLOBAL, POWER_PRESS, saved[1])
                if (saved.size > 2 && saved[2].isNotEmpty()) {
                    put(SYSTEM, BRIGHTNESS_MODE, saved[2])
                    relight()
                }
                Log.i(TAG, "Put back the screen settings a stopped session left held")
            } catch (e: IOException) {
                Log.w(TAG, "Couldn't read $SAVED", e)
            }
            SAVED.delete()
        }

        private fun relight() {
            run("cmd", "display", "power-off", "0")
            run("cmd", "display", "power-reset", "0")
        }

        /**
         * Every input device reporting [capability]: more than one may, as the power key does on a
         * Pixel - the button's own gpio_keys, and the touchscreen for its wake gesture.
         */
        private fun inputDevices(capability: String): List<String> {
            val devices = mutableListOf<String>()
            var device: String? = null
            for (line in run("getevent", "-pl").split("\n")) {
                if (line.startsWith("add device")) {
                    device = line.substringAfter(':').trim()
                } else if (device != null && capability in line && device !in devices) {
                    devices += device
                }
            }
            return devices
        }

        private fun parse(value: String?, fallback: Long): Long {
            val parsed = value?.toLongOrNull() ?: return fallback
            return if (parsed > 0 && parsed < Int.MAX_VALUE) parsed else fallback
        }

        /** A setting, or null where unset. */
        private fun get(namespace: String, key: String): String? {
            val value = run("settings", "get", namespace, key).trim()
            return if (value.isEmpty() || value == "null") null else value
        }

        private fun put(namespace: String, key: String, value: String?) {
            if (value.isNullOrEmpty() || value == "null") {
                run("settings", "delete", namespace, key)
            } else {
                run("settings", "put", namespace, key, value)
            }
        }

        private fun run(vararg command: String): String {
            try {
                val process = ProcessBuilder(*command).redirectErrorStream(true).start()
                val out = String(process.inputStream.readBytes())
                process.waitFor()
                return out
            } catch (e: IOException) {
                Log.w(TAG, "Could not run ${command.joinToString(" ")}", e)
            } catch (e: InterruptedException) {
                Thread.currentThread().interrupt()
            }
            return ""
        }
    }

    /** One input event: its type, code and value, as the kernel's struct input_event has them. */
    private fun interface InputEvent {
        fun on(type: Int, code: Int, value: Int)
    }

    private val power = shellContext.getSystemService(Context.POWER_SERVICE) as PowerManager
    private val displays: DisplayManager =
        DisplayManager::class.java.getDeclaredConstructor(Context::class.java).newInstance(shellContext)
    private var ticker: Thread? = null
    /** What stops each watcher. */
    private var touches: Runnable? = null
    private var keys: Runnable? = null
    @Volatile private var lastTouch = 0L
    @Volatile private var panelOff = false
    private var idleMs = 0L
    private var savedTimeout: String? = null
    private var savedPowerPress: String? = null
    /** The user's brightness mode, held while the panel is dark. */
    private var brightnessWhileDark: String? = null

    @Synchronized
    fun start() {
        if (ticker != null) return
        restoreLeftovers()
        savedTimeout = get(SYSTEM, TIMEOUT)
        savedPowerPress = get(GLOBAL, POWER_PRESS)
        if (!save(null)) {
            Log.w(TAG, "Can't keep the screen settings safe; leaving them alone")
            return
        }
        idleMs = parse(savedTimeout, DEFAULT_TIMEOUT_MS)
        put(SYSTEM, TIMEOUT, HELD_TIMEOUT)
        lastTouch = SystemClock.uptimeMillis()
        touches = watch("ABS_MT_POSITION_X") { _, _, _ -> touched() }
        keys = watch("KEY_POWER") { type, code, value ->
            if (type == EV_KEY && code == KEY_POWER && value == 1) {
                Log.i(TAG, "Power key pressed")
                powerPressed()
            }
        }
        // The press is only ours to take where we can see it; otherwise it sleeps as ever.
        if (keys != null) put(GLOBAL, POWER_PRESS, POWER_PRESS_NOTHING)
        wakeForSession()
        ticker = Thread(::tick, "slipstream-panel").also { it.start() }
        Log.i(TAG, "Keeping the phone awake; its panel goes off after ${idleMs}ms idle or at the power button")
    }

    /**
     * A session started with the phone asleep (in a pocket, as it would be) gets no touches
     * through, so the phone wakes - dark, as it was. Also the lock screen goes where it can: it
     * hides every app on the unit's display but the launcher, which may show over it. That's only
     * where it isn't secured, or Smart Lock trusts the unit; elsewhere this just asks for unlock.
     */
    @Synchronized
    private fun wakeForSession() {
        val asleep = !power.isInteractive
        if (asleep) run("input", "keyevent", "KEYCODE_WAKEUP")
        run("wm", "dismiss-keyguard")
        if (asleep) {
            dark()
            Log.i(TAG, "Woke the phone for the session, dark")
        }
    }

    @Synchronized
    fun stop() {
        val t = ticker ?: return
        t.interrupt()
        ticker = null
        touches?.run()
        keys?.run()
        touches = null
        keys = null
        if (panelOff) lit()
        restoreLeftovers()
    }

    private fun tick() {
        while (!Thread.currentThread().isInterrupted) {
            synchronized(this) {
                if (!power.isInteractive) {
                    // Asleep after all (a long press's Power off, say): start over once awake.
                    if (panelOff) lit()
                    lastTouch = SystemClock.uptimeMillis()
                } else if (panelOff) {
                    // A brightness change re-applies the panel's state; keep it off.
                    if (phoneState() != Display.STATE_OFF) run("cmd", "display", "power-off", "0")
                } else if (SystemClock.uptimeMillis() - lastTouch >= idleMs) {
                    dark()
                }
            }
            try {
                Thread.sleep(TICK_MS)
            } catch (e: InterruptedException) {
                return
            }
        }
    }

    @Synchronized
    private fun touched() {
        lastTouch = SystemClock.uptimeMillis()
        if (panelOff) {
            lit()
            Log.i(TAG, "Phone panel back on at a touch")
        }
    }

    /** With the short press left to us: dark if lit, lit if we darkened it. */
    @Synchronized
    private fun powerPressed() {
        lastTouch = SystemClock.uptimeMillis()
        if (panelOff) {
            lit()
            Log.i(TAG, "Phone panel back on at the power button")
        } else if (power.isInteractive && phoneState() == Display.STATE_ON) {
            dark()
        }
        // Otherwise asleep: Android wakes it as ever.
    }

    private fun phoneState() =
        displays.getDisplay(Display.DEFAULT_DISPLAY)?.state ?: Display.STATE_UNKNOWN

    /** Off, with manual brightness meanwhile: auto-brightness would keep re-lighting it. */
    private fun dark() {
        panelOff = true
        brightnessWhileDark = get(SYSTEM, BRIGHTNESS_MODE)
        save(brightnessWhileDark ?: "")
        put(SYSTEM, BRIGHTNESS_MODE, "0")
        run("cmd", "display", "power-off", "0")
        Log.i(TAG, "Phone panel off; the unit carries on")
    }

    /**
     * Back as it was: the brightness mode the user had, and the panel's proper state. power-reset
     * alone is refused (silently: "runnable is null") when the display manager has the panel down
     * as on already - nothing to change, as it sees it. Off first makes the reset a change it acts
     * on. (Not a sleep and wake: that brings up the lock screen, over the unit's apps.)
     */
    private fun lit() {
        panelOff = false
        put(SYSTEM, BRIGHTNESS_MODE, brightnessWhileDark)
        save(null)
        relight()
    }

    private fun save(brightness: String?): Boolean = try {
        SAVED.writeText("${savedTimeout.orEmpty()}\n${savedPowerPress.orEmpty()}\n${brightness.orEmpty()}")
        true
    } catch (e: IOException) {
        Log.w(TAG, "Couldn't write $SAVED", e)
        false
    }

    /**
     * Follows the input device that reports [capability] (in getevent's listing), read straight
     * from its node - getevent's own output is block-buffered through a pipe, holding a single
     * key press back indefinitely. Returns what stops it, or null where there's none.
     */
    private fun watch(capability: String, onEvent: InputEvent): Runnable? {
        val stops = inputDevices(capability).mapNotNull { watchDevice(it, capability, onEvent) }
        if (stops.isEmpty()) {
            Log.w(TAG, "No input device with $capability")
            return null
        }
        return Runnable { stops.forEach { it.run() } }
    }

    private fun watchDevice(device: String, capability: String, onEvent: InputEvent): Runnable? {
        val input = try {
            FileInputStream(device)
        } catch (e: IOException) {
            Log.w(TAG, "Can't watch $device", e)
            return null
        }
        Log.i(TAG, "Watching $device for $capability")
        val stopped = AtomicBoolean()
        val reader = Thread({
            val event = ByteArray(EVENT_SIZE)
            val fields = ByteBuffer.wrap(event).order(ByteOrder.LITTLE_ENDIAN)
            input.use {
                try {
                    while (!stopped.get()) {
                        var read = 0
                        while (read < EVENT_SIZE) {
                            val n = it.read(event, read, EVENT_SIZE - read)
                            if (n < 0) return@Thread
                            read += n
                        }
                        if (!stopped.get()) {
                            onEvent.on(
                                fields.getShort(16).toInt() and 0xffff,
                                fields.getShort(18).toInt() and 0xffff,
                                fields.getInt(20),
                            )
                        }
                    }
                } catch (ignored: IOException) {
                }
            }
        }, "slipstream-$capability")
        // A read blocks until the next event, so a stopped watcher may linger until then.
        reader.isDaemon = true
        reader.start()
        return Runnable { stopped.set(true) }
    }
}
