package io.github.cfraz89.slipstream.launcher

import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.ColorFilter
import android.graphics.Outline
import android.graphics.Paint
import android.graphics.PixelFormat
import android.graphics.drawable.Drawable
import android.graphics.drawable.LayerDrawable
import android.view.View
import java.lang.ref.WeakReference
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

    private var blurred: Bitmap? = null
    private val origin = IntArray(2)
    private val panels = mutableListOf<WeakReference<View>>()

    /** The wallpaper as [wallpaper] shows it, centre-cropped, blurred to [radius] px. */
    fun setWallpaper(drawable: Drawable?, wallpaper: View, radius: Float) {
        val width = wallpaper.width
        val height = wallpaper.height
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
        blurred = small
        wallpaper.getLocationInWindow(origin)
        panels.removeAll { it.get() == null }
        panels.forEach { it.get()?.background?.invalidateSelf() }
    }

    /** Puts the blurred wallpaper under [view]'s own background, clipped to its outline. */
    fun apply(view: View) {
        val background = view.background ?: return
        if (background is LayerDrawable && background.getDrawable(0) is Backdrop) return
        view.background = LayerDrawable(arrayOf(Backdrop(view), background))
        view.clipToOutline = true
        panels.removeAll { it.get() == null || it.get() === view }
        panels += WeakReference(view)
    }

    fun clear(view: View) {
        panels.removeAll { it.get() == null || it.get() === view }
    }

    /** Draws the slice of the blurred wallpaper that lies behind its view. */
    private class Backdrop(view: View) : Drawable() {
        private val view = WeakReference(view)
        private val paint = Paint(Paint.FILTER_BITMAP_FLAG)
        private val drawnAt = IntArray(2)

        init {
            // A panel moved by a layout would otherwise carry its old slice with it.
            view.addOnLayoutChangeListener { _, _, _, _, _, _, _, _, _ -> invalidateSelf() }
        }

        override fun draw(canvas: Canvas) {
            val bitmap = blurred ?: return
            val v = view.get() ?: return
            v.getLocationInWindow(drawnAt)
            canvas.save()
            canvas.translate(
                (origin[0] - drawnAt[0]).toFloat(),
                (origin[1] - drawnAt[1]).toFloat(),
            )
            canvas.scale(SCALE.toFloat(), SCALE.toFloat())
            canvas.drawBitmap(bitmap, 0f, 0f, paint)
            canvas.restore()
        }

        /** None of its own, so the panel's shape gives the outline it is clipped to. */
        override fun getOutline(outline: Outline) = outline.setEmpty()

        override fun setAlpha(alpha: Int) {
            paint.alpha = alpha
        }

        override fun setColorFilter(colorFilter: ColorFilter?) {
            paint.colorFilter = colorFilter
        }

        @Deprecated("Deprecated in Java")
        override fun getOpacity() = PixelFormat.TRANSLUCENT
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
