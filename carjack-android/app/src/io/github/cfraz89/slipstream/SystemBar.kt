package io.github.cfraz89.slipstream

import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Path
import android.graphics.RectF
import android.os.BatteryManager
import android.telephony.SignalStrength
import android.telephony.TelephonyCallback
import android.telephony.TelephonyDisplayInfo
import android.telephony.TelephonyManager
import android.util.Log
import kotlin.math.roundToInt

/**
 * A bar along the bottom of the head unit's picture: back and home, for where its switchgear can't
 * do either, the phone's mobile signal and battery and the temperature outside, each a setting. Drawn into the
 * stream by the [Relay] below the display, rather than as a window on it, so it sits over any app;
 * touches on it are taken before they reach the display.
 */
object SystemBar {
    private const val TAG = "SlipstreamBar"
    const val KEYCODE_BACK = 4
    /** Of the view's height: about 48dp on a 7" unit. */
    private const val HEIGHT_FRACTION = 1 / 14f
    private const val MIN_HEIGHT = 40

    /** No button pressed, for [render]. */
    const val NONE = 0
    const val BACK = 1
    const val HOME = 2

    /** What the bar can show, each a setting, all on by default. */
    enum class Part(val pref: String, val title: String) {
        BUTTONS("system_bar_buttons", "Back and home buttons"),
        SIGNAL("system_bar_signal", "Mobile signal"),
        BATTERY("system_bar_battery", "Phone battery"),
        TEMPERATURE("system_bar_temperature", "Ambient temperature"),
        ;

        fun shown(context: Context) = Variant.setting(context, pref, "true").toBoolean()

        fun setShown(context: Context, on: Boolean) {
            Variant.prefs(context).edit().putString(pref, on.toString()).commit()
        }
    }

    private const val PREF = "system_bar"

    fun isOn(context: Context) = Variant.setting(context, PREF, "true").toBoolean()

    fun setOn(context: Context, on: Boolean) {
        Variant.prefs(context).edit().putString(PREF, on.toString()).commit()
    }

    fun parts(context: Context) = Part.entries.filter { it.shown(context) }.toSet()

    /**
     * The bar's height in panel pixels; 0 where it's off or shows nothing, or where the display
     * isn't Android's own.
     */
    fun height(context: Context, viewHeight: Int): Int {
        if (!Variant.showsAndroid(context) || !isOn(context) || parts(context).isEmpty()) return 0
        return maxOf(MIN_HEIGHT, (viewHeight * HEIGHT_FRACTION).roundToInt())
    }

    /**
     * What the bar reports: the battery's charge and whether it's charging, the mobile signal's
     * level (0-4, null without service) and generation, and the temperature.
     */
    data class Status(
        val battery: Int?,
        val charging: Boolean,
        val temperature: String?,
        val signal: Int? = null,
        val network: String? = null,
    )

    /** The last temperature read, for drawing without waiting on the network. */
    @Volatile private var temperature: String? = null

    @Volatile private var signal: Int? = null
    /** 5G riding on 4G, which the network's own type calls LTE. */
    @Volatile private var nrOverride = false
    private var listening = false

    /** The battery and signal now and the temperature as last read: cheap. */
    fun status(context: Context): Status {
        val intent: Intent? = context.registerReceiver(null, IntentFilter(Intent.ACTION_BATTERY_CHANGED))
        val level = intent?.getIntExtra(BatteryManager.EXTRA_LEVEL, -1) ?: -1
        val scale = intent?.getIntExtra(BatteryManager.EXTRA_SCALE, 100) ?: 100
        val charging = (intent?.getIntExtra(BatteryManager.EXTRA_PLUGGED, 0) ?: 0) != 0
        listen(context)
        return Status(
            if (level < 0) null else level * 100 / scale, charging, temperature,
            signal, network(context),
        )
    }

    /** Follows the signal and 5G, from the first [status] on, for the process's life. */
    @Synchronized
    private fun listen(context: Context) {
        if (listening) return
        listening = true
        val phone = context.getSystemService(TelephonyManager::class.java) ?: return
        signal = phone.signalStrength?.level
        val executor = context.mainExecutor
        try {
            phone.registerTelephonyCallback(executor, object : TelephonyCallback(), TelephonyCallback.SignalStrengthsListener {
                override fun onSignalStrengthsChanged(strength: SignalStrength) {
                    signal = strength.level
                }
            })
        } catch (e: SecurityException) {
            Log.w(TAG, "No signal strength: ${e.message}")
        }
        try {
            phone.registerTelephonyCallback(executor, object : TelephonyCallback(), TelephonyCallback.DisplayInfoListener {
                override fun onDisplayInfoChanged(info: TelephonyDisplayInfo) {
                    nrOverride = info.overrideNetworkType == TelephonyDisplayInfo.OVERRIDE_NETWORK_TYPE_NR_NSA ||
                        info.overrideNetworkType == TelephonyDisplayInfo.OVERRIDE_NETWORK_TYPE_NR_ADVANCED
                }
            })
        } catch (e: SecurityException) {
            Log.w(TAG, "No 5G indication: ${e.message}")
        }
    }

    /**
     * The mobile network's generation - 5G, 4G and so on - from its data side, else its voice side;
     * null where neither is known or it can't be read.
     */
    private fun network(context: Context): String? {
        val type = try {
            val phone = context.getSystemService(TelephonyManager::class.java) ?: return null
            phone.dataNetworkType.takeIf { it != TelephonyManager.NETWORK_TYPE_UNKNOWN } ?: phone.voiceNetworkType
        } catch (_: SecurityException) {
            null
        }
        return when (type) {
            TelephonyManager.NETWORK_TYPE_NR -> "5G"
            TelephonyManager.NETWORK_TYPE_LTE, TelephonyManager.NETWORK_TYPE_IWLAN -> if (nrOverride) "5G" else "4G"
            TelephonyManager.NETWORK_TYPE_HSPAP -> "H+"
            TelephonyManager.NETWORK_TYPE_HSPA, TelephonyManager.NETWORK_TYPE_HSDPA,
            TelephonyManager.NETWORK_TYPE_HSUPA -> "H"
            TelephonyManager.NETWORK_TYPE_UMTS, TelephonyManager.NETWORK_TYPE_TD_SCDMA,
            TelephonyManager.NETWORK_TYPE_EVDO_0, TelephonyManager.NETWORK_TYPE_EVDO_A,
            TelephonyManager.NETWORK_TYPE_EVDO_B, TelephonyManager.NETWORK_TYPE_EHRPD -> "3G"
            TelephonyManager.NETWORK_TYPE_EDGE -> "E"
            TelephonyManager.NETWORK_TYPE_GPRS, TelephonyManager.NETWORK_TYPE_CDMA,
            TelephonyManager.NETWORK_TYPE_1xRTT, TelephonyManager.NETWORK_TYPE_GSM -> "G"
            else -> null
        }
    }

    /** The temperature read afresh where it's due. Blocks: off the main thread. */
    fun refreshTemperature(context: Context) {
        Variant.temperature(context)?.let { temperature = it }
    }

    /** Where the back and home buttons' middles sit, in bar heights from its left. */
    private const val BACK_X = 1.2f
    private const val HOME_X = 3.2f
    /** Where back's touch ends and home's begins, and where home's ends, in bar heights. */
    private const val BUTTONS_SPLIT = 2.2f
    private const val BUTTONS_END = 4.2f

    /**
     * The bar as drawn: dark, [parts] on it - back and home left; the signal, battery and
     * temperature right, in that order - and [pressed] ([BACK] or [HOME]) on a lit pill.
     */
    fun render(width: Int, height: Int, parts: Set<Part>, status: Status, pressed: Int = NONE): Bitmap {
        val bar = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
        val canvas = Canvas(bar)
        canvas.drawColor(Color.rgb(0x12, 0x14, 0x18))
        val fg = Color.rgb(0xe2, 0xe2, 0xe9)
        val icon = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = fg }
        val text = Paint(Paint.ANTI_ALIAS_FLAG).apply {
            color = fg
            textSize = height * 0.36f
        }
        val size = height * 0.36f
        val y = height / 2f
        val margin = height * 0.5f
        val baseline = y - (text.descent() + text.ascent()) / 2

        if (Part.BUTTONS in parts) {
            if (pressed != NONE) {
                val x = height * if (pressed == BACK) BACK_X else HOME_X
                val halfWidth = height * 0.9f
                val halfHeight = height * 0.38f
                val lit = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = Color.rgb(0x3a, 0x3f, 0x4a) }
                canvas.drawRoundRect(x - halfWidth, y - halfHeight, x + halfWidth, y + halfHeight, halfHeight, halfHeight, lit)
            }
            val backX = height * BACK_X
            val back = Path().apply {
                moveTo(backX - size * 0.6f, y)
                lineTo(backX + size * 0.5f, y - size * 0.6f)
                lineTo(backX + size * 0.5f, y + size * 0.6f)
                close()
            }
            canvas.drawPath(back, icon)
            val ring = Paint(icon).apply {
                style = Paint.Style.STROKE
                strokeWidth = height * 0.07f
            }
            canvas.drawCircle(height * HOME_X, y, size * 0.6f, ring)
        }

        // Right to left from the right edge: temperature, battery, signal.
        val gap = height * 0.4f
        var right = width - margin
        if (Part.TEMPERATURE in parts) {
            status.temperature?.let {
                val w = text.measureText(it)
                canvas.drawText(it, right - w, baseline, text)
                right -= w + gap
            }
        }

        if (Part.BATTERY in parts && status.battery != null) {
            val label = "${status.battery}%"
            val labelWidth = text.measureText(label)
            canvas.drawText(label, right - labelWidth, baseline, text)
            right -= labelWidth + height * 0.15f
            right -= drawBattery(canvas, right, y, height, status, fg) + gap
        }

        if (Part.SIGNAL in parts) {
            right -= drawSignal(canvas, right, y, height, status.signal, fg)
            status.network?.takeIf { status.signal != null }?.let {
                val small = Paint(text).apply { textSize = height * 0.28f }
                val w = small.measureText(it)
                right -= height * 0.12f
                canvas.drawText(it, right - w, y - (small.descent() + small.ascent()) / 2, small)
            }
        }
        return bar
    }

    /** Four rising bars ending at [right], [level] of them lit; none without service. Its width. */
    private fun drawSignal(canvas: Canvas, right: Float, y: Float, height: Int, level: Int?, color: Int): Float {
        val barsHeight = height * 0.38f
        val barWidth = height * 0.075f
        val space = height * 0.045f
        val bottom = y + barsHeight / 2
        val lit = Paint(Paint.ANTI_ALIAS_FLAG).apply { this.color = color }
        val dim = Paint(lit).apply { alpha = 0x50 }
        val width = 4 * barWidth + 3 * space
        val left = right - width
        val corner = barWidth * 0.3f
        for (i in 0 until 4) {
            val x = left + i * (barWidth + space)
            val top = bottom - barsHeight * (i + 1) / 4
            canvas.drawRoundRect(x, top, x + barWidth, bottom, corner, corner, if (level != null && i < level) lit else dim)
        }
        return width
    }

    /**
     * A battery on its side, [right] its nub's end, filled to its charge, a bolt over it while
     * charging. Its width.
     */
    private fun drawBattery(canvas: Canvas, right: Float, y: Float, height: Int, status: Status, color: Int): Float {
        val bodyHeight = height * 0.32f
        val bodyWidth = bodyHeight * 1.9f
        val nub = bodyHeight * 0.14f
        val stroke = height * 0.04f
        val body = RectF(right - nub - bodyWidth, y - bodyHeight / 2, right - nub, y + bodyHeight / 2)
        val outline = Paint(Paint.ANTI_ALIAS_FLAG).apply {
            this.color = color
            style = Paint.Style.STROKE
            strokeWidth = stroke
        }
        val solid = Paint(Paint.ANTI_ALIAS_FLAG).apply { this.color = color }
        val corner = bodyHeight * 0.2f
        canvas.drawRoundRect(body, corner, corner, outline)
        canvas.drawRect(right - nub, y - bodyHeight * 0.22f, right, y + bodyHeight * 0.22f, solid)
        val inset = stroke * 1.5f
        val level = (status.battery ?: 0).coerceIn(0, 100) / 100f
        val fill = RectF(body.left + inset, body.top + inset, body.left + inset + (body.width() - 2 * inset) * level, body.bottom - inset)
        if (fill.width() > 0) canvas.drawRect(fill, solid)
        if (status.charging) {
            // A bolt across the body, cut out of the fill in the bar's own colour, then outlined.
            val cx = body.centerX()
            val h = bodyHeight * 1.1f
            val bolt = Path().apply {
                moveTo(cx + h * 0.12f, y - h / 2)
                lineTo(cx - h * 0.22f, y + h * 0.06f)
                lineTo(cx - h * 0.02f, y + h * 0.06f)
                lineTo(cx - h * 0.12f, y + h / 2)
                lineTo(cx + h * 0.22f, y - h * 0.06f)
                lineTo(cx + h * 0.02f, y - h * 0.06f)
                close()
            }
            canvas.drawPath(bolt, Paint(Paint.ANTI_ALIAS_FLAG).apply { this.color = Color.rgb(0x12, 0x14, 0x18) })
            canvas.drawPath(bolt, Paint(outline).apply { strokeWidth = stroke * 0.8f })
        }
        return right - body.left + stroke / 2
    }

    /**
     * [display] for touches above the bar; on it, back and home where [buttons] shows them - each
     * button's stretch of the bar's left end - done when the finger lifts where it went down.
     * [shown] gets the button to show pressed, or [NONE] once it's let go.
     */
    fun touch(
        display: Touch, barX: Int, barY: Int, barWidth: Int, barHeight: Int, buttons: Boolean,
        back: Runnable, home: Runnable, shown: (Int) -> Unit = {},
    ): Touch = object : Touch {
        /** Per pointer: which button it went down on, or [NONE]; [OFF] for the bar elsewhere. */
        private val pressed = IntArray(16)
        private val OFF = -1
        /** Per pointer: down on the display, so its touch goes there. */
        private val onDisplay = BooleanArray(16)

        override fun touch(action: Int, pointer: Int, panelX: Float, panelY: Float) {
            val slot = Math.floorMod(pointer, pressed.size)
            val onBar = panelY >= barY && panelY < barY + barHeight &&
                panelX >= barX && panelX < barX + barWidth
            val x = (panelX - barX) / barHeight
            val button = when {
                !onBar -> NONE
                !buttons -> OFF
                x < BUTTONS_SPLIT -> BACK
                x < BUTTONS_END -> HOME
                else -> OFF
            }
            if (action == Touch.DOWN) {
                onDisplay[slot] = !onBar
                pressed[slot] = button
                if (button == BACK || button == HOME) shown(button)
            }
            if (onDisplay[slot]) {
                display.touch(action, pointer, panelX, panelY)
                return
            }
            if (action == Touch.UP) {
                val down = pressed[slot]
                if ((down == BACK || down == HOME) && button == down) (if (down == BACK) back else home).run()
                if (down == BACK || down == HOME) shown(NONE)
                pressed[slot] = NONE
            }
        }
    }
}
