package io.github.cfraz89.slipstream

import android.graphics.Bitmap
import android.graphics.Matrix
import android.graphics.SurfaceTexture
import android.opengl.EGL14
import android.opengl.EGLConfig
import android.opengl.EGLContext
import android.opengl.EGLDisplay
import android.opengl.EGLExt
import android.opengl.EGLSurface
import android.opengl.GLES11Ext
import android.opengl.GLES20
import android.opengl.GLUtils
import android.os.Handler
import android.os.HandlerThread
import android.os.SystemClock
import android.view.Surface
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.FloatBuffer
import java.util.concurrent.CountDownLatch

/**
 * Draws the display's frames into the encoder's input: into part of it, for an accessory that keeps
 * a strip of its panel for itself (the stream covers the whole panel, the picture only the view),
 * and again on a still screen, which would otherwise leave the encoder with nothing to send.
 */
class Relay(
    private val output: Surface,
    private val width: Int,
    private val height: Int,
    private val viewX: Int,
    private val viewY: Int,
    private val viewWidth: Int,
    private val viewHeight: Int,
    /** Under the view, where wanted; null for none. */
    private val bar: Bitmap?,
) {
    private companion object {
        const val VERTEX = """
attribute vec2 pos;
uniform mat4 tex;
varying vec2 uv;
void main() {
  uv = (tex * vec4(pos * 0.5 + 0.5, 0.0, 1.0)).xy;
  gl_Position = vec4(pos, 0.0, 1.0);
}
"""
        const val FRAGMENT = """
#extension GL_OES_EGL_image_external : require
precision mediump float;
uniform samplerExternalOES frame;
varying vec2 uv;
void main() { gl_FragColor = texture2D(frame, uv); }
"""
        val QUAD = floatArrayOf(-1f, -1f, 1f, -1f, -1f, 1f, 1f, 1f)
        /** The [NavBar]: a plain bitmap texture, rows top down as a Bitmap has them. */
        const val BAR_VERTEX = """
attribute vec2 pos;
varying vec2 uv;
void main() {
  uv = vec2(pos.x * 0.5 + 0.5, 0.5 - pos.y * 0.5);
  gl_Position = vec4(pos, 0.0, 1.0);
}
"""
        const val BAR_FRAGMENT = """
precision mediump float;
uniform sampler2D bar;
varying vec2 uv;
void main() { gl_FragColor = texture2D(bar, uv); }
"""
        /** A [Cues] badge, faded by alpha; the bitmap uploads premultiplied. */
        const val CUE_FRAGMENT = """
precision mediump float;
uniform sampler2D cue;
uniform float alpha;
varying vec2 uv;
void main() { gl_FragColor = texture2D(cue, uv) * alpha; }
"""
        const val CUE_MS = 450L
        const val CUE_FRAME_MS = 33L
        const val IDLE_REDRAW_MS = 500L
        const val CAPTURE_WIDTH = 480

        fun link(vertex: String, fragment: String): Int {
            val program = GLES20.glCreateProgram()
            GLES20.glAttachShader(program, compile(GLES20.GL_VERTEX_SHADER, vertex))
            GLES20.glAttachShader(program, compile(GLES20.GL_FRAGMENT_SHADER, fragment))
            GLES20.glLinkProgram(program)
            return program
        }

        fun compile(type: Int, source: String): Int {
            val shader = GLES20.glCreateShader(type)
            GLES20.glShaderSource(shader, source)
            GLES20.glCompileShader(shader)
            return shader
        }

        fun linearTexture(target: Int) {
            GLES20.glTexParameteri(target, GLES20.GL_TEXTURE_MIN_FILTER, GLES20.GL_LINEAR)
            GLES20.glTexParameteri(target, GLES20.GL_TEXTURE_MAG_FILTER, GLES20.GL_LINEAR)
        }
    }

    private val thread = HandlerThread("carjack-relay")
    private lateinit var handler: Handler
    private lateinit var eglDisplay: EGLDisplay
    private lateinit var eglContext: EGLContext
    private lateinit var eglSurface: EGLSurface
    private var texture: SurfaceTexture? = null
    private var input: Surface? = null
    private var program = 0
    /** GL reads the vertices from here at every draw, so it must outlive setUp. */
    private lateinit var quad: FloatBuffer
    private val matrix = FloatArray(16)
    private var haveFrame = false
    private var lastDrawMs = 0L
    private var lastTimestampNs = 0L
    private var pendingCapture: ((Bitmap) -> Unit)? = null
    private var barProgram = 0
    private var cueProgram = 0
    private var cueTexture = 0
    private var cueWidth = 0
    private var cueHeight = 0
    private var cueMs = CUE_MS
    /** When the showing cue started; 0 for none. */
    private var cueStartMs = 0L

    /** Returns the surface the display should render into. */
    fun start(): Surface {
        thread.start()
        handler = Handler(thread.looper)
        val ready = CountDownLatch(1)
        handler.post {
            setUp()
            ready.countDown()
        }
        handler.postDelayed(idleRedraw, IDLE_REDRAW_MS)
        try {
            ready.await()
        } catch (e: InterruptedException) {
            Thread.currentThread().interrupt()
        }
        return input!!
    }

    private fun setUp() {
        eglDisplay = EGL14.eglGetDisplay(EGL14.EGL_DEFAULT_DISPLAY)
        val version = IntArray(2)
        EGL14.eglInitialize(eglDisplay, version, 0, version, 1)
        val attribs = intArrayOf(
            EGL14.EGL_RED_SIZE, 8, EGL14.EGL_GREEN_SIZE, 8, EGL14.EGL_BLUE_SIZE, 8,
            EGL14.EGL_RENDERABLE_TYPE, EGL14.EGL_OPENGL_ES2_BIT,
            EGLExt.EGL_RECORDABLE_ANDROID, 1, EGL14.EGL_NONE,
        )
        val configs = arrayOfNulls<EGLConfig>(1)
        val count = IntArray(1)
        EGL14.eglChooseConfig(eglDisplay, attribs, 0, configs, 0, 1, count, 0)
        eglContext = EGL14.eglCreateContext(
            eglDisplay, configs[0], EGL14.EGL_NO_CONTEXT,
            intArrayOf(EGL14.EGL_CONTEXT_CLIENT_VERSION, 2, EGL14.EGL_NONE), 0,
        )
        eglSurface = EGL14.eglCreateWindowSurface(
            eglDisplay, configs[0], output, intArrayOf(EGL14.EGL_NONE), 0,
        )
        EGL14.eglMakeCurrent(eglDisplay, eglSurface, eglSurface, eglContext)

        program = link(VERTEX, FRAGMENT)
        val tex = IntArray(1)
        GLES20.glGenTextures(1, tex, 0)
        GLES20.glBindTexture(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, tex[0])
        linearTexture(GLES11Ext.GL_TEXTURE_EXTERNAL_OES)
        val surfaceTexture = SurfaceTexture(tex[0])
        surfaceTexture.setDefaultBufferSize(viewWidth, viewHeight)
        surfaceTexture.setOnFrameAvailableListener({ draw() }, handler)
        texture = surfaceTexture
        input = Surface(surfaceTexture)

        quad = ByteBuffer.allocateDirect(QUAD.size * 4)
            .order(ByteOrder.nativeOrder()).asFloatBuffer().put(QUAD)
        quad.position(0)
        attachQuad(program)

        if (bar != null) {
            // On texture unit 1, so the display's frame keeps unit 0 to itself.
            barProgram = link(BAR_VERTEX, BAR_FRAGMENT)
            val barTexture = IntArray(1)
            GLES20.glGenTextures(1, barTexture, 0)
            GLES20.glActiveTexture(GLES20.GL_TEXTURE1)
            GLES20.glBindTexture(GLES20.GL_TEXTURE_2D, barTexture[0])
            linearTexture(GLES20.GL_TEXTURE_2D)
            GLUtils.texImage2D(GLES20.GL_TEXTURE_2D, 0, bar, 0)
            GLES20.glActiveTexture(GLES20.GL_TEXTURE0)
            GLES20.glUseProgram(barProgram)
            GLES20.glUniform1i(GLES20.glGetUniformLocation(barProgram, "bar"), 1)
            attachQuad(barProgram)
        }

        // Texture unit 2, beside the bar's.
        cueProgram = link(BAR_VERTEX, CUE_FRAGMENT)
        val cueTextures = IntArray(1)
        GLES20.glGenTextures(1, cueTextures, 0)
        cueTexture = cueTextures[0]
        GLES20.glActiveTexture(GLES20.GL_TEXTURE2)
        GLES20.glBindTexture(GLES20.GL_TEXTURE_2D, cueTexture)
        linearTexture(GLES20.GL_TEXTURE_2D)
        GLES20.glActiveTexture(GLES20.GL_TEXTURE0)
        GLES20.glUseProgram(cueProgram)
        GLES20.glUniform1i(GLES20.glGetUniformLocation(cueProgram, "cue"), 2)
        attachQuad(cueProgram)
        GLES20.glUseProgram(program)
    }

    private fun attachQuad(program: Int) {
        val pos = GLES20.glGetAttribLocation(program, "pos")
        GLES20.glEnableVertexAttribArray(pos)
        GLES20.glVertexAttribPointer(pos, 2, GLES20.GL_FLOAT, false, 0, quad)
    }

    /** Flashes [badge] low in the middle of the view, fading out over [ms]. */
    fun cue(badge: Bitmap, ms: Long = CUE_MS) {
        handler.post {
            if (texture == null) return@post
            GLES20.glActiveTexture(GLES20.GL_TEXTURE2)
            GLES20.glBindTexture(GLES20.GL_TEXTURE_2D, cueTexture)
            GLUtils.texImage2D(GLES20.GL_TEXTURE_2D, 0, badge, 0)
            GLES20.glActiveTexture(GLES20.GL_TEXTURE0)
            cueWidth = badge.width
            cueHeight = badge.height
            cueMs = ms
            val showing = cueStartMs != 0L
            cueStartMs = SystemClock.uptimeMillis()
            if (!showing) handler.post(cueFrame)
        }
    }

    /** A still screen sends no frames, so the fade draws its own. */
    private val cueFrame = object : Runnable {
        override fun run() {
            if (texture == null || cueStartMs == 0L) return
            render(System.nanoTime())
            if (cueStartMs != 0L) handler.postDelayed(this, CUE_FRAME_MS)
        }
    }

    private fun draw() {
        val t = texture ?: return
        t.updateTexImage()
        t.getTransformMatrix(matrix)
        haveFrame = true
        render(t.timestamp)
    }

    /**
     * A still screen stops producing frames, and the encoder repeats the last one only a few
     * times; a unit left without frames shows black. So the last frame is drawn again when the
     * display has been idle a while.
     */
    private val idleRedraw = object : Runnable {
        override fun run() {
            if (texture == null) return
            // Before the display's first frame too: black, but the session can start on it.
            if (SystemClock.uptimeMillis() - lastDrawMs >= IDLE_REDRAW_MS) render(System.nanoTime())
            handler.postDelayed(this, IDLE_REDRAW_MS)
        }
    }

    /** The picture as the unit next gets it, [CAPTURE_WIDTH] wide, to [sink]. */
    fun capture(sink: (Bitmap) -> Unit) {
        handler.post {
            if (texture == null || !haveFrame) return@post
            pendingCapture = sink
            render(System.nanoTime())
        }
    }

    /** The view, the right way up: GL's rows run bottom to top. */
    private fun readView(): Bitmap {
        val pixels = ByteBuffer.allocateDirect(viewWidth * viewHeight * 4)
        GLES20.glReadPixels(
            viewX, height - viewY - viewHeight, viewWidth, viewHeight,
            GLES20.GL_RGBA, GLES20.GL_UNSIGNED_BYTE, pixels,
        )
        val raw = Bitmap.createBitmap(viewWidth, viewHeight, Bitmap.Config.ARGB_8888)
        raw.copyPixelsFromBuffer(pixels)
        val scale = CAPTURE_WIDTH.toFloat() / viewWidth
        val flip = Matrix().apply { setScale(scale, -scale) }
        val upright = Bitmap.createBitmap(raw, 0, 0, viewWidth, viewHeight, flip, true)
        raw.recycle()
        return upright
    }

    private fun render(timestamp: Long) {
        // Strictly increasing, whether the time came from the display or the clock.
        val timestampNs = maxOf(timestamp, lastTimestampNs + 1)
        lastTimestampNs = timestampNs
        lastDrawMs = SystemClock.uptimeMillis()
        GLES20.glViewport(0, 0, width, height)
        GLES20.glClearColor(0f, 0f, 0f, 1f)
        GLES20.glClear(GLES20.GL_COLOR_BUFFER_BIT)
        // Without a frame from the display yet, the texture holds nothing to draw.
        if (haveFrame) {
            // GL's origin is bottom left; the view's is top left.
            GLES20.glViewport(viewX, height - viewY - viewHeight, viewWidth, viewHeight)
            GLES20.glUniformMatrix4fv(
                GLES20.glGetUniformLocation(program, "tex"), 1, false, matrix, 0,
            )
            GLES20.glDrawArrays(GLES20.GL_TRIANGLE_STRIP, 0, 4)
        }
        if (bar != null) {
            GLES20.glViewport(viewX, height - viewY - viewHeight - bar.height, viewWidth, bar.height)
            GLES20.glUseProgram(barProgram)
            GLES20.glDrawArrays(GLES20.GL_TRIANGLE_STRIP, 0, 4)
            GLES20.glUseProgram(program)
            GLES20.glViewport(viewX, height - viewY - viewHeight, viewWidth, viewHeight)
        }
        if (cueStartMs != 0L) {
            val elapsed = SystemClock.uptimeMillis() - cueStartMs
            if (elapsed >= cueMs) {
                cueStartMs = 0
            } else {
                val x = viewX + (viewWidth - cueWidth) / 2
                val top = viewY + viewHeight * 3 / 4 - cueHeight / 2
                GLES20.glViewport(x, height - top - cueHeight, cueWidth, cueHeight)
                GLES20.glEnable(GLES20.GL_BLEND)
                GLES20.glBlendFunc(GLES20.GL_ONE, GLES20.GL_ONE_MINUS_SRC_ALPHA)
                GLES20.glUseProgram(cueProgram)
                GLES20.glUniform1f(
                    GLES20.glGetUniformLocation(cueProgram, "alpha"),
                    1f - elapsed.toFloat() / cueMs,
                )
                GLES20.glDrawArrays(GLES20.GL_TRIANGLE_STRIP, 0, 4)
                GLES20.glUseProgram(program)
                GLES20.glDisable(GLES20.GL_BLEND)
                GLES20.glViewport(viewX, height - viewY - viewHeight, viewWidth, viewHeight)
            }
        }
        pendingCapture?.let {
            it(readView())
            pendingCapture = null
        }
        EGLExt.eglPresentationTimeANDROID(eglDisplay, eglSurface, timestampNs)
        EGL14.eglSwapBuffers(eglDisplay, eglSurface)
    }

    fun stop() {
        val done = CountDownLatch(1)
        handler.post {
            texture?.release()
            texture = null
            input?.release()
            EGL14.eglMakeCurrent(
                eglDisplay, EGL14.EGL_NO_SURFACE, EGL14.EGL_NO_SURFACE, EGL14.EGL_NO_CONTEXT,
            )
            EGL14.eglDestroySurface(eglDisplay, eglSurface)
            EGL14.eglDestroyContext(eglDisplay, eglContext)
            EGL14.eglTerminate(eglDisplay)
            done.countDown()
        }
        try {
            done.await()
        } catch (e: InterruptedException) {
            Thread.currentThread().interrupt()
        }
        thread.quitSafely()
    }
}
