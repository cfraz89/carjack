package io.github.cfraz89.slipstream

import android.annotation.SuppressLint
import android.graphics.SurfaceTexture
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.view.MotionEvent
import android.view.Surface
import android.view.TextureView
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.ui.Modifier
import androidx.compose.ui.viewinterop.AndroidView
import io.github.cfraz89.slipstream.launcher.LauncherPreview
import java.util.concurrent.Executors

/**
 * Slipstream mode's display here on the phone, as a session would build it for the head unit - the scale,
 * the chosen app, the system bar, touch - but the size of the space under the title bar. It
 * closes once the app on it has gone: Slipstream's launcher is asked to leave on back. Not during
 * a session, which has the display.
 */
class PreviewActivity : ComponentActivity() {
    private companion object {
        const val WATCH_MS = 500L
    }

    private val main = Handler(Looper.getMainLooper())
    private val background = Executors.newSingleThreadExecutor()
    private var output: Surface? = null
    private var relay: Relay? = null
    private var closeDisplay: Runnable? = null
    private var touch: Touch? = null
    /** An app has been seen on the display, so its going is the app leaving. */
    private var seenApp = false

    @SuppressLint("ClickableViewAccessibility")
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        if (PhoneRemoteActivity.connected) {
            Toast.makeText(this, "Not while the head unit's connected", Toast.LENGTH_SHORT).show()
            finish()
            return
        }
        window.setDecorFitsSystemWindows(false)
        LauncherPreview.onExit = { runOnUiThread(::finish) }
        setContent {
            SlipstreamTheme {
                Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.surface) {
                    Column(Modifier.fillMaxSize().safeDrawingPadding()) {
                        TopBar("Preview", ::finish)
                        AndroidView(
                            factory = { context ->
                                TextureView(context).apply {
                                    surfaceTextureListener = textureListener
                                    setOnTouchListener { _, event -> touched(event) }
                                }
                            },
                            modifier = Modifier.fillMaxWidth().weight(1f),
                        )
                    }
                }
            }
        }
    }

    private val textureListener = object : TextureView.SurfaceTextureListener {
        override fun onSurfaceTextureAvailable(texture: SurfaceTexture, width: Int, height: Int) {
            val out = Surface(texture)
            output = out
            val bar = SystemBar.height(this@PreviewActivity, height)
            val displayHeight = height - bar
            val parts = SystemBar.parts(this@PreviewActivity)
            val status = SystemBar.status(this@PreviewActivity)
            val r = Relay(
                out, width, height, 0, 0, width, displayHeight,
                if (bar == 0) null else SystemBar.render(width, bar, parts, status),
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
                    SystemBar.touch(
                        display, 0, displayHeight, width, bar, SystemBar.Part.BUTTONS in parts,
                        { Variant.key(this@PreviewActivity, SystemBar.KEYCODE_BACK) },
                        { Variant.goHome(this@PreviewActivity) },
                        { r.bar(SystemBar.render(width, bar, parts, SystemBar.status(this@PreviewActivity), it)) },
                    )
                }
                // Then the temperature, which may wait on the network.
                if (bar != 0 && SystemBar.Part.TEMPERATURE in parts) {
                    SystemBar.refreshTemperature(this@PreviewActivity)
                    r.bar(SystemBar.render(width, bar, parts, SystemBar.status(this@PreviewActivity)))
                }
                main.postDelayed(watchApp, WATCH_MS)
            }
        }

        override fun onSurfaceTextureSizeChanged(texture: SurfaceTexture, width: Int, height: Int) {}

        /**
         * Not waited for: the relay may be held up in a frame only this thread takes in. The texture
         * going (returning true) fails that frame, and the relay stops.
         */
        override fun onSurfaceTextureDestroyed(texture: SurfaceTexture): Boolean {
            main.removeCallbacks(watchApp)
            touch = null
            // On the opening's thread, after it: a display still opening is closed too.
            background.execute {
                closeDisplay?.run()
                closeDisplay = null
                relay?.stop()
                relay = null
            }
            output?.release()
            output = null
            return true
        }

        override fun onSurfaceTextureUpdated(texture: SurfaceTexture) {}
    }

    /** Closes once the app the display showed has gone from it; the launcher says so itself, sooner. */
    private val watchApp = object : Runnable {
        override fun run() {
            // A check already under way when the preview closed comes back after it.
            if (isDestroyed || background.isShutdown) return
            background.execute {
                val top = ShizukuDisplay.topPackage()
                main.post {
                    if (isDestroyed) return@post
                    if (top != null) {
                        seenApp = true
                    } else if (seenApp) {
                        finish()
                        return@post
                    }
                    if (closeDisplay != null) main.postDelayed(this, WATCH_MS)
                }
            }
        }
    }

    /** The phone's fingers as the head unit's: the view is the display's own size. */
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
        LauncherPreview.onExit = null
        main.removeCallbacks(watchApp)
        background.shutdown()
        super.onDestroy()
    }
}
