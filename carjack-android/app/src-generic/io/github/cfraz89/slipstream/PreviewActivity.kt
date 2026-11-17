package io.github.cfraz89.slipstream

import android.app.Activity
import android.os.Bundle
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.widget.Toast
import java.util.concurrent.Executors

/**
 * Open mode's display here on the phone, as a session would build it for the head unit - the scale,
 * the chosen app, the navigation bar, touch - but the size of the phone's screen. Not during a
 * session, which has the display.
 */
class PreviewActivity : Activity() {
    private val background = Executors.newSingleThreadExecutor()
    private var relay: Relay? = null
    private var closeDisplay: Runnable? = null
    private var touch: Touch? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        if (DashboardActivity.connected) {
            Toast.makeText(this, "Not while the head unit's connected", Toast.LENGTH_SHORT).show()
            finish()
            return
        }
        val surface = SurfaceView(this)
        surface.holder.addCallback(callback)
        surface.setOnTouchListener { _, event -> touched(event) }
        setContentView(surface)
    }

    private val callback = object : SurfaceHolder.Callback {
        override fun surfaceCreated(holder: SurfaceHolder) {}

        /** Its first size is the display's; a rotation recreates the screen. */
        override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
            if (relay != null) return
            val output = holder.surface
            val bar = NavBar.height(this@PreviewActivity, height)
            val displayHeight = height - bar
            val r = Relay(
                output, width, height, 0, 0, width, displayHeight,
                if (bar == 0) null else NavBar.render(width, bar),
            )
            relay = r
            background.execute {
                val density = ScreenStream.density(this@PreviewActivity, width, displayHeight)
                val close = Variant.openPreview(this@PreviewActivity, width, displayHeight, density, r.start())
                if (close == null) {
                    runOnUiThread {
                        Toast.makeText(this@PreviewActivity, "Shizuku isn't running", Toast.LENGTH_SHORT).show()
                        finish()
                    }
                    return@execute
                }
                closeDisplay = close
                val display = Variant.touch(this@PreviewActivity, 0, 0, width, displayHeight)
                touch = if (bar == 0) {
                    display
                } else {
                    NavBar.touch(
                        display, 0, displayHeight, width, bar,
                        { Variant.key(this@PreviewActivity, NavBar.KEYCODE_BACK) },
                        { Variant.goHome(this@PreviewActivity) },
                    )
                }
            }
        }

        override fun surfaceDestroyed(holder: SurfaceHolder) {
            touch = null
            // Waits, so the display is off the surface before it goes.
            background.submit {
                closeDisplay?.run()
                closeDisplay = null
                relay?.stop()
                relay = null
            }.get()
        }
    }

    /** The phone's fingers as the head unit's: the surface is the display's own size. */
    private fun touched(event: MotionEvent): Boolean {
        val t = touch ?: return true
        fun send(action: Int, index: Int) = t.touch(action, event.getPointerId(index), event.getX(index), event.getY(index))
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN, MotionEvent.ACTION_POINTER_DOWN -> send(Touch.DOWN, event.actionIndex)
            MotionEvent.ACTION_UP, MotionEvent.ACTION_POINTER_UP -> send(Touch.UP, event.actionIndex)
            MotionEvent.ACTION_MOVE -> for (i in 0 until event.pointerCount) send(Touch.MOVE, i)
            MotionEvent.ACTION_CANCEL -> for (i in 0 until event.pointerCount) send(Touch.UP, i)
        }
        return true
    }

    override fun onDestroy() {
        background.shutdown()
        super.onDestroy()
    }
}
