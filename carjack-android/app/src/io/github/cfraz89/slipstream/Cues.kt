package io.github.cfraz89.slipstream

import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Path

/**
 * Badges the [Relay] flashes over the picture when the unit's switchgear goes back or selects: in
 * an app that shows no focus, nothing else would answer the press.
 */
object Cues {
    /** A left-pointing chevron on a dark disc. */
    fun back(size: Int): Bitmap {
        val badge = disc(size)
        val c = size / 2f
        val arm = size * 0.16f
        val chevron = Path().apply {
            moveTo(c + arm * 0.5f, c - arm)
            lineTo(c - arm * 0.6f, c)
            lineTo(c + arm * 0.5f, c + arm)
        }
        Canvas(badge).drawPath(chevron, icon(size))
        return badge
    }

    /** A filled dot in a ring on a dark disc. */
    fun select(size: Int): Bitmap {
        val badge = disc(size)
        val canvas = Canvas(badge)
        val icon = icon(size)
        val c = size / 2f
        canvas.drawCircle(c, c, size * 0.2f, icon)
        icon.style = Paint.Style.FILL
        canvas.drawCircle(c, c, size * 0.08f, icon)
        return badge
    }

    /** [text] on a dark pill [height] tall, as wide as the text needs. */
    fun label(text: String, height: Int): Bitmap {
        val paint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
            color = Color.rgb(0xe2, 0xe2, 0xe9)
            textSize = height * 0.42f
            isFakeBoldText = true
            letterSpacing = 0.08f
        }
        val width = (paint.measureText(text) + height).toInt()
        val badge = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
        val canvas = Canvas(badge)
        val fill = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = Color.argb(0xcc, 0x12, 0x14, 0x18) }
        canvas.drawRoundRect(0f, 0f, width.toFloat(), height.toFloat(), height / 2f, height / 2f, fill)
        val baseline = height / 2f - (paint.descent() + paint.ascent()) / 2
        canvas.drawText(text, height / 2f, baseline, paint)
        return badge
    }

    private fun disc(size: Int): Bitmap {
        val badge = Bitmap.createBitmap(size, size, Bitmap.Config.ARGB_8888)
        val fill = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = Color.argb(0xcc, 0x12, 0x14, 0x18) }
        Canvas(badge).drawCircle(size / 2f, size / 2f, size / 2f, fill)
        return badge
    }

    private fun icon(size: Int) = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        color = Color.rgb(0xe2, 0xe2, 0xe9)
        style = Paint.Style.STROKE
        strokeWidth = size * 0.07f
        strokeCap = Paint.Cap.ROUND
        strokeJoin = Paint.Join.ROUND
    }
}
