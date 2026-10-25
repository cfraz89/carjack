package io.github.cfraz89.slipstream

import android.content.Context
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Path
import kotlin.math.roundToInt

/**
 * A back and home bar along the bottom of the unit's picture, for where its switchgear can't do
 * either. Drawn into the stream by the [Relay] below the display, rather than as a window on it,
 * so it sits over any app; touches on it are taken before they reach the display.
 */
object NavBar {
    const val KEYCODE_BACK = 4
    /** Of the view's height: about 48dp on a 7" unit. */
    private const val HEIGHT_FRACTION = 1 / 14f
    private const val MIN_HEIGHT = 40

    /** The bar's height in panel pixels; 0 where it's off. */
    fun height(context: Context, viewHeight: Int): Int {
        if (!Variant.setting(context, "nav_bar", "false").toBoolean()) return 0
        return maxOf(MIN_HEIGHT, (viewHeight * HEIGHT_FRACTION).roundToInt())
    }

    /** The bar as drawn: dark, a back arrow left of centre and a home ring right of it. */
    fun render(width: Int, height: Int): Bitmap {
        val bar = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
        val canvas = Canvas(bar)
        canvas.drawColor(Color.rgb(0x12, 0x14, 0x18))
        val icon = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = Color.rgb(0xe2, 0xe2, 0xe9) }
        val size = height * 0.36f
        val y = height / 2f

        val backX = width / 3f
        val back = Path().apply {
            moveTo(backX - size * 0.6f, y)
            lineTo(backX + size * 0.5f, y - size * 0.6f)
            lineTo(backX + size * 0.5f, y + size * 0.6f)
            close()
        }
        canvas.drawPath(back, icon)

        icon.style = Paint.Style.STROKE
        icon.strokeWidth = height * 0.07f
        canvas.drawCircle(width * 2 / 3f, y, size * 0.6f, icon)
        return bar
    }

    /**
     * [display] for touches above the bar; on it, the left half is back and the right half home,
     * done when the finger lifts where it went down.
     */
    fun touch(
        display: Touch, barX: Int, barY: Int, barWidth: Int, barHeight: Int,
        back: Runnable, home: Runnable,
    ): Touch = object : Touch {
        /** Per pointer: which button it went down on (1 back, 2 home), or 0 for the display. */
        private val pressed = IntArray(16)

        override fun touch(action: Int, pointer: Int, panelX: Float, panelY: Float) {
            val slot = Math.floorMod(pointer, pressed.size)
            val onBar = panelY >= barY && panelY < barY + barHeight &&
                panelX >= barX && panelX < barX + barWidth
            val button = when {
                !onBar -> 0
                panelX < barX + barWidth / 2f -> 1
                else -> 2
            }
            if (action == Touch.DOWN) pressed[slot] = button
            if (pressed[slot] == 0) {
                display.touch(action, pointer, panelX, panelY)
                return
            }
            if (action == Touch.UP) {
                if (button == pressed[slot]) (if (button == 1) back else home).run()
                pressed[slot] = 0
            }
        }
    }
}
