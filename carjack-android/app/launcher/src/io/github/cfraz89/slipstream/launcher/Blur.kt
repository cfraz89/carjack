package io.github.cfraz89.slipstream.launcher

import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.drawable.Drawable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.composed
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.drawscope.scale
import androidx.compose.ui.graphics.drawscope.translate
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.layout.positionInWindow
import kotlin.math.max
import kotlin.math.roundToInt

/**
 * Backdrop blur for the panels over the wallpaper, on public API alone.
 *
 * The system launcher's View.setBackdropRenderEffect is hidden from an ordinary app; but on home
 * all that lies behind a panel is the wallpaper, so a blurred copy of it, drawn at each panel's
 * place, looks the same. HomeActivity hands the wallpaper over as it lays it out.
 */
object Blur {
    /** The copy is blurred at a quarter size: cheaper, and the blur hides the scaling. */
    private const val SCALE = 4
    private const val PASSES = 3

    /** Snapshot state, so every panel showing - in any of the launcher's windows - redraws. */
    private var blurred by mutableStateOf<ImageBitmap?>(null)
    private var origin = Offset.Zero

    /**
     * The wallpaper as home shows it - [width] by [height] at [at] in the window, centre-cropped
     * - blurred to [radius] px.
     */
    fun setWallpaper(drawable: Drawable?, width: Int, height: Int, at: Offset, radius: Float) {
        if (drawable == null || width == 0 || height == 0) return
        val small = Bitmap.createBitmap(
            max(1, width / SCALE), max(1, height / SCALE), Bitmap.Config.ARGB_8888,
        )
        val canvas = Canvas(small)
        canvas.scale(small.width.toFloat() / width, small.height.toFloat() / height)
        val dw = drawable.intrinsicWidth.takeIf { it > 0 } ?: width
        val dh = drawable.intrinsicHeight.takeIf { it > 0 } ?: height
        val crop = max(width.toFloat() / dw, height.toFloat() / dh)
        val left = ((width - dw * crop) / 2).roundToInt()
        val top = ((height - dh * crop) / 2).roundToInt()
        val bounds = drawable.copyBounds()
        drawable.setBounds(left, top, left + (dw * crop).roundToInt(), top + (dh * crop).roundToInt())
        drawable.draw(canvas)
        drawable.bounds = bounds

        // Three box passes come close to a Gaussian of the same reach.
        boxBlur(small, max(1, (radius / SCALE / 2).roundToInt()))
        origin = at
        blurred = small.asImageBitmap()
    }

    /**
     * Draws the slice of the blurred wallpaper that lies behind this panel; clip the panel to its
     * shape before this.
     */
    fun Modifier.backdrop(): Modifier = composed {
        var at by remember { mutableStateOf(Offset.Zero) }
        onGloballyPositioned { at = it.positionInWindow() }
            .drawBehind {
                val image = blurred ?: return@drawBehind
                translate(origin.x - at.x, origin.y - at.y) {
                    scale(SCALE.toFloat(), pivot = Offset.Zero) { drawImage(image) }
                }
            }
    }

    private fun boxBlur(bitmap: Bitmap, radius: Int) {
        val w = bitmap.width
        val h = bitmap.height
        val src = IntArray(w * h)
        val tmp = IntArray(w * h)
        bitmap.getPixels(src, 0, w, 0, 0, w, h)
        repeat(PASSES) {
            pass(src, tmp, w, h, radius, 1, w)
            pass(tmp, src, h, w, radius, w, 1)
        }
        bitmap.setPixels(src, 0, w, 0, 0, w, h)
    }

    /**
     * One running-sum box pass along [lines] lines of [length] pixels, [step] apart within a line
     * and [stride] apart between lines; edges clamp.
     */
    private fun pass(
        src: IntArray, dst: IntArray, length: Int, lines: Int, radius: Int, step: Int, stride: Int,
    ) {
        val span = 2 * radius + 1
        for (line in 0 until lines) {
            val base = line * stride
            var a = 0
            var r = 0
            var g = 0
            var b = 0
            for (i in -radius..radius) {
                val p = src[base + i.coerceIn(0, length - 1) * step]
                a += p ushr 24
                r += (p shr 16) and 0xff
                g += (p shr 8) and 0xff
                b += p and 0xff
            }
            for (i in 0 until length) {
                dst[base + i * step] =
                    ((a / span) shl 24) or ((r / span) shl 16) or ((g / span) shl 8) or (b / span)
                val out = src[base + (i - radius).coerceIn(0, length - 1) * step]
                val inn = src[base + (i + radius + 1).coerceIn(0, length - 1) * step]
                a += (inn ushr 24) - (out ushr 24)
                r += ((inn shr 16) and 0xff) - ((out shr 16) and 0xff)
                g += ((inn shr 8) and 0xff) - ((out shr 8) and 0xff)
                b += (inn and 0xff) - (out and 0xff)
            }
        }
    }
}
