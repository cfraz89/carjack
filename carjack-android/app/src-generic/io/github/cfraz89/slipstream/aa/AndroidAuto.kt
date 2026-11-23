package io.github.cfraz89.slipstream.aa

import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Typeface
import android.text.Layout
import android.text.StaticLayout
import android.text.TextPaint
import android.net.wifi.WifiManager
import android.os.Handler
import android.os.HandlerThread
import android.os.PowerManager
import android.view.KeyEvent
import android.view.Surface
import io.github.cfraz89.slipstream.Action
import io.github.cfraz89.slipstream.Input
import io.github.cfraz89.slipstream.Mappings
import io.github.cfraz89.slipstream.Presses
import io.github.cfraz89.slipstream.Profiles
import io.github.cfraz89.slipstream.ScreenStream
import io.github.cfraz89.slipstream.Variant
import io.github.cfraz89.slipstream.aa.proto.Control.Service.MediaSinkService.VideoConfiguration
import java.io.IOException
import java.net.InetSocketAddress
import java.net.Socket
import kotlin.math.roundToInt

/**
 * Android Auto on the head unit, outside Slipstream mode: Slipstream connects to the head unit server
 * Android Auto runs on the phone and plays the car, its video decoded into the stream and the head
 * unit's touches and keys sent back. Works with the phone locked and its screen off. Android Auto's
 * developer settings have to have started the server since the phone last booted.
 */
object AndroidAuto {
    private const val PORT = 5277
    private const val CONNECT_TIMEOUT_MS = 800
    private const val RETRY_MS = 2_000L
    /** How often a missing server is logged, in tries. */
    private const val LOG_EVERY = 15

    @Volatile private var running = false
    @Volatile private var session: AaSession? = null
    private var dialler: Thread? = null
    private var wakeLock: PowerManager.WakeLock? = null
    private var wifiLock: WifiManager.WifiLock? = null
    private var geometry: AaGeometry? = null
    private var contentWidth = 0
    private var contentHeight = 0

    /** Whether it's what the head unit shows, so touches and keys go to it. */
    val active get() = running

    const val PACKAGE = "com.google.android.projection.gearhead"

    /** Android Auto's own settings, where its developer settings and the server's switch are. */
    fun settings(): Intent = Intent("$PACKAGE.SETTINGS").setPackage(PACKAGE)

    fun installed(context: Context) = try {
        context.packageManager.getPackageInfo(PACKAGE, 0)
        true
    } catch (e: PackageManager.NameNotFoundException) {
        false
    }

    /**
     * Whether the last try at Android Auto's head unit server found it: null before any. Not probed
     * for: a connection hung up unused leaves the server taking no more until it's restarted.
     */
    fun serverFound(context: Context): Boolean? {
        val prefs = Profiles.global(context)
        return if (prefs.contains(SERVER_FOUND)) prefs.getBoolean(SERVER_FOUND, false) else null
    }

    private const val SERVER_FOUND = "aa_server_found"

    private fun found(context: Context, found: Boolean) {
        Profiles.global(context).edit().putBoolean(SERVER_FOUND, found).apply()
    }

    /**
     * Android Auto into [target], a [width]x[height] view, until the returned Runnable closes it.
     * It reconnects whenever its session ends.
     */
    @Synchronized
    fun open(context: Context, width: Int, height: Int, target: Surface): Runnable {
        val g = geometry(width, height)
        geometry = g
        val app = context.applicationContext
        wakeLock = app.getSystemService(PowerManager::class.java)
            .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "carjack:androidauto").also { it.acquire() }
        @Suppress("DEPRECATION")
        wifiLock = app.getSystemService(WifiManager::class.java)
            .createWifiLock(WifiManager.WIFI_MODE_FULL_HIGH_PERF, "carjack:androidauto").also { it.acquire() }
        running = true
        dialler = Thread({ dial(app, g, target) }, "carjack-aa-dial").apply { start() }
        AaLog.i("Asking for ${g.frameWidth}x${g.frameHeight}, margins ${g.marginWidth}x${g.marginHeight}, ${g.density}dpi")
        return Runnable { close() }
    }

    @Synchronized
    private fun close() {
        running = false
        dialler?.interrupt()
        dialler = null
        session?.stop()
        session = null
        wakeLock?.release()
        wakeLock = null
        wifiLock?.release()
        wifiLock = null
    }

    private fun dial(context: Context, g: AaGeometry, initial: Surface) {
        var target = initial
        /** The notice drawn into [target], which leaves it taken: a decoder needs a fresh one. */
        var noticeShown = false
        var tries = 0
        try {
            while (running) {
                if (session == null) {
                    val socket = Socket()
                    try {
                        socket.connect(InetSocketAddress("127.0.0.1", PORT), CONNECT_TIMEOUT_MS)
                        AaLog.i("Connected to Android Auto's head unit server")
                        tries = 0
                        found(context, true)
                        if (noticeShown) {
                            ScreenStream.current?.newInputSurface()?.let { target = it }
                            noticeShown = false
                        }
                    } catch (e: IOException) {
                        socket.close()
                        found(context, false)
                        if (!noticeShown) noticeShown = showNotice(target)
                        if (tries++ % LOG_EVERY == 0) {
                            AaLog.w(
                                "No head unit server on :$PORT; in Android Auto's settings tap Version " +
                                    "10 times, then ⋮ → Start head unit server",
                            )
                        }
                        Thread.sleep(RETRY_MS)
                        continue
                    }
                    try {
                        start(context, socket, g, target)
                    } catch (e: Exception) {
                        AaLog.e("Couldn't start a session", e)
                        socket.close()
                    }
                }
                Thread.sleep(RETRY_MS)
            }
        } catch (ignored: InterruptedException) {
        }
    }

    /**
     * What's wrong and how to fix it, on the head unit, while there's no head unit server: else
     * it shows black. False where the surface can't be drawn on.
     */
    private fun showNotice(target: Surface): Boolean {
        val canvas = try {
            target.lockCanvas(null)
        } catch (e: Exception) {
            AaLog.w("Couldn't show the notice: $e")
            return false
        }
        val width = canvas.width.toFloat()
        val height = canvas.height.toFloat()
        canvas.drawColor(Color.rgb(0x11, 0x13, 0x18))
        val title = TextPaint(Paint.ANTI_ALIAS_FLAG).apply {
            color = Color.rgb(0xe2, 0xe2, 0xe9)
            textSize = minOf(width, height) * 0.065f
            typeface = Typeface.DEFAULT_BOLD
        }
        val body = TextPaint(Paint.ANTI_ALIAS_FLAG).apply {
            color = Color.rgb(0xc3, 0xc6, 0xcf)
            textSize = minOf(width, height) * 0.042f
        }
        val margin = width * 0.1f
        val textWidth = (width - margin * 2).toInt()
        fun layout(text: String, paint: TextPaint) =
            StaticLayout.Builder.obtain(text, 0, text.length, paint, textWidth)
                .setAlignment(Layout.Alignment.ALIGN_CENTER)
                .setLineSpacing(0f, 1.2f)
                .build()
        val heading = layout("Android Auto isn't ready", title)
        val steps = layout(
            "Start Android Auto's head unit server on the phone: in Android Auto's settings, tap " +
                "Version 10 times, then ⋮ → Start head unit server.\n\nOr switch Slipstream to " +
                "Slipstream mode.",
            body,
        )
        val gap = title.textSize
        var y = (height - heading.height - gap - steps.height) / 2
        canvas.save()
        canvas.translate(margin, y)
        heading.draw(canvas)
        canvas.restore()
        y += heading.height + gap
        canvas.save()
        canvas.translate(margin, y)
        steps.draw(canvas)
        canvas.restore()
        target.unlockCanvasAndPost(canvas)
        return true
    }

    @Synchronized
    private fun start(context: Context, socket: Socket, g: AaGeometry, target: Surface) {
        if (!running) {
            socket.close()
            return
        }
        val decoder = VideoDecoder(target, g.frameWidth, g.frameHeight)
        lateinit var started: AaSession
        // Unlocked: close() holds the lock while it ends a session.
        started = AaSession(AapLink(socket, context), decoder, g) {
            if (session === started) session = null
        }
        session = started
        ScreenStream.current?.crop(contentWidth.toFloat() / g.frameWidth, contentHeight.toFloat() / g.frameHeight)
        started.start()
    }

    /**
     * The coded frame of the head unit's orientation, and the margins that leave its middle the
     * view's shape. The density puts 480dp across the short side, as the display gets.
     */
    private fun geometry(width: Int, height: Int): AaGeometry {
        val portrait = height > width
        val (frameWidth, frameHeight) = if (portrait) 720 to 1280 else 1280 to 720
        // The widest box of the view's shape the frame holds.
        val aspect = width.toFloat() / height
        if (aspect > frameWidth.toFloat() / frameHeight) {
            contentWidth = frameWidth
            contentHeight = (frameWidth / aspect).roundToInt()
        } else {
            contentHeight = frameHeight
            contentWidth = (frameHeight * aspect).roundToInt()
        }
        return AaGeometry(
            if (portrait) VideoConfiguration.VideoCodecResolutionType._720x1280 else VideoConfiguration.VideoCodecResolutionType._1280x720,
            frameWidth, frameHeight,
            frameWidth - contentWidth, frameHeight - contentHeight,
            minOf(contentWidth, contentHeight) * 160 / ScreenStream.SMALLEST_WIDTH_DP,
        )
    }

    /**
     * A touch in the view's pixels, [viewX] and [viewY] its corner on the panel, into Android Auto's
     * content: it takes touches from the content's corner, not the frame's, margins left out.
     */
    fun touch(action: Int, pointer: Int, panelX: Float, panelY: Float, viewX: Int, viewY: Int, viewWidth: Int, viewHeight: Int) {
        val s = session ?: return
        val x = ((panelX - viewX) * contentWidth / viewWidth).roundToInt()
        val y = ((panelY - viewY) * contentHeight / viewHeight).roundToInt()
        s.touch(action, pointer, x.coerceIn(0, contentWidth - 1), y.coerceIn(0, contentHeight - 1))
    }

    /** Keys in order, off the caller's thread, for [presses] to time. */
    private val keys = Handler(HandlerThread("slipstream-aa-keys").apply { start() }.looper)
    private val presses = Presses(keys) { action, down -> perform(action, down) }

    /** A switchgear key, as the profile's mapping maps it; media keys as they are. */
    fun key(context: Context, code: Int, down: Boolean) {
        keys.post {
            val input = Input.of(code)
            if (input == null) {
                session?.key(code, down)
            } else {
                presses.press(Mappings.active(context), input, down)
            }
        }
    }

    /** Back or home from the phone remote's own buttons, not the switchgear's: as they are, unmapped. */
    fun navigate(code: Int, down: Boolean) {
        keys.post { session?.key(code, down) }
    }

    private fun perform(action: Action, down: Boolean) {
        val s = session ?: return
        val mapped = when (action) {
            // Without a rotary controller, which moves Android Auto's rail to the bottom.
            Action.NEXT -> KeyEvent.KEYCODE_DPAD_DOWN
            Action.PREVIOUS -> KeyEvent.KEYCODE_DPAD_UP
            Action.UP -> KeyEvent.KEYCODE_DPAD_UP
            Action.DOWN -> KeyEvent.KEYCODE_DPAD_DOWN
            Action.LEFT -> KeyEvent.KEYCODE_DPAD_LEFT
            Action.RIGHT -> KeyEvent.KEYCODE_DPAD_RIGHT
            Action.SELECT -> KeyEvent.KEYCODE_DPAD_CENTER
            Action.BACK -> KeyEvent.KEYCODE_BACK
            Action.HOME -> KeyEvent.KEYCODE_HOME
            Action.VOICE -> KeyEvent.KEYCODE_SEARCH
            Action.NONE -> return
        }
        s.key(mapped, down)
    }
}
