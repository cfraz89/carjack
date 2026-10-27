package io.github.cfraz89.slipstream

import android.content.Context
import android.graphics.Bitmap
import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaCodecList
import android.media.MediaFormat
import android.os.Bundle
import android.os.Handler
import android.os.HandlerThread
import android.os.Looper
import android.os.SystemClock
import android.util.Log
import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.nio.ByteBuffer
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import kotlin.math.roundToInt

/**
 * The display the [Variant] provides, sized to the part of the accessory's panel it gets, into a
 * hardware encoder whose output goes to the core.
 */
class ScreenStream(
    private val context: Context,
    private val session: Long,
    private val onError: Runnable,
) {
    companion object {
        /** The stream running now, for the switchgear's cues; null between sessions. */
        @Volatile var current: ScreenStream? = null
            private set

        private const val TAG = "carjack"
        /** The UI's smallest width, in dp, unless the `density` setting says otherwise. */
        const val SMALLEST_WIDTH_DP = 480
        const val REPEAT_FRAME_US = 100_000L
        const val DEFAULT_BITRATE = 8_000_000
        /** What the Linux build gives the CFMOTO, which shows its HEVC: 3 Mbit/s VBR, IDR every 2 s. */
        const val HEVC_BITRATE = 3_000_000
        const val STALL_MS = 10_000L
        const val WATCHDOG_INTERVAL_MS = 2_000L
        const val FIRST_PREVIEW_MS = 6_000L
        const val PREVIEW_INTERVAL_MS = 60_000L
        /** A cue badge's size, as a fraction of the view's height. */
        const val CUE_FRACTION = 0.14f
        /** A flashed label's height, likewise, and how long it shows. */
        const val LABEL_FRACTION = 0.09f
        const val FLASH_MS = 1200L
        /** How much of each session's start is kept, in encoder buffers: the config and ~6 s. */
        const val SAMPLE_BUFFERS = 60
        const val KEPT_SAMPLES = 6

        /**
         * Ask for what head units decode: HEVC Main at a variable bitrate, as the Linux build
         * sends it, and H.264 Constrained Baseline (else Baseline) at a constant one, rather than
         * the High profile phone encoders default to. A unit handed a profile it can't decode
         * stops reading the stream. Each only where the encoder offers it.
         */
        fun applyProfile(
            format: MediaFormat, caps: MediaCodecInfo.CodecCapabilities, hevc: Boolean,
        ) {
            val wanted = if (hevc) {
                listOf(MediaCodecInfo.CodecProfileLevel.HEVCProfileMain)
            } else {
                listOf(
                    MediaCodecInfo.CodecProfileLevel.AVCProfileConstrainedBaseline,
                    MediaCodecInfo.CodecProfileLevel.AVCProfileBaseline,
                )
            }
            wanted.firstOrNull { profile -> caps.profileLevels.any { it.profile == profile } }
                ?.let { format.setInteger(MediaFormat.KEY_PROFILE, it) }
            val mode = if (hevc) {
                MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_VBR
            } else {
                MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_CBR
            }
            if (caps.encoderCapabilities?.isBitrateModeSupported(mode) == true) {
                format.setInteger(MediaFormat.KEY_BITRATE_MODE, mode)
            }
        }

        /**
         * A hardware encoder that takes this exact size, else any encoder that does; null leaves
         * the choice to the platform. A panel like 1080x600 isn't 16-line aligned, which some
         * reject.
         */
        fun pickEncoder(mime: String, width: Int, height: Int): String? {
            var fallback: String? = null
            for (info in MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos) {
                if (!info.isEncoder) continue
                for (type in info.supportedTypes) {
                    if (!type.equals(mime, ignoreCase = true)) continue
                    val video = info.getCapabilitiesForType(type).videoCapabilities
                    if (video == null || !video.isSizeSupported(width, height)) continue
                    if (info.isHardwareAccelerated) return info.name
                    if (fallback == null) fallback = info.name
                }
            }
            if (fallback == null) Log.w(TAG, "No $mime encoder claims ${width}x$height")
            return fallback
        }
    }

    private val thread = HandlerThread("carjack-encoder")
    @Volatile private var codec: MediaCodec? = null
    /** Closes the display the [Variant] opened. */
    private var closeDisplay: Runnable? = null
    private var relay: Relay? = null
    /** Off the encoder's thread, which is what the watchdog watches. */
    private val watchdogHandler = Handler(Looper.getMainLooper())
    @Volatile private var lastOutput = 0L
    private var backCue: Bitmap? = null
    private var selectCue: Bitmap? = null
    private var sampled = 0
    private var sample: FileOutputStream? = null

    /**
     * The panel is [width]x[height]; the display shows in the view rectangle, with [NavBar]
     * [barHeight] high under it (0 for none).
     */
    fun start(
        width: Int, height: Int, fps: Int, viewX: Int, viewY: Int, viewWidth: Int,
        viewHeight: Int, barHeight: Int, hevc: Boolean,
    ) {
        val density = intSetting(
            "density", maxOf(120, minOf(viewWidth, viewHeight) * 160 / SMALLEST_WIDTH_DP),
        )

        val mime = if (hevc) MediaFormat.MIMETYPE_VIDEO_HEVC else MediaFormat.MIMETYPE_VIDEO_AVC
        val format = MediaFormat.createVideoFormat(mime, width, height).apply {
            setInteger(
                MediaFormat.KEY_COLOR_FORMAT, MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface,
            )
            setInteger(
                MediaFormat.KEY_BIT_RATE, intSetting("bitrate", if (hevc) HEVC_BITRATE else DEFAULT_BITRATE),
            )
            setInteger(MediaFormat.KEY_FRAME_RATE, fps)
            setFloat(MediaFormat.KEY_MAX_FPS_TO_ENCODER, fps.toFloat())
            setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, if (hevc) 2 else 10)
            // A still screen still has to answer keyframe requests.
            setLong(MediaFormat.KEY_REPEAT_PREVIOUS_FRAME_AFTER, REPEAT_FRAME_US)
            setInteger(MediaFormat.KEY_PRIORITY, 0)
        }

        thread.start()
        val encoder = pickEncoder(mime, width, height)
        val c = if (encoder != null) {
            MediaCodec.createByCodecName(encoder)
        } else {
            MediaCodec.createEncoderByType(mime)
        }
        codec = c
        applyProfile(format, c.codecInfo.getCapabilitiesForType(mime), hevc)
        Log.i(
            TAG,
            "Encoding ${width}x$height@$fps with ${c.name}; display ${viewWidth}x$viewHeight at " +
                "($viewX,$viewY), ${density}dpi",
        )
        c.setCallback(callback, Handler(thread.looper))
        c.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
        val input = c.createInputSurface()
        c.start()

        // Always through the relay: besides placing the view, it keeps a still screen sending.
        val r = Relay(
            input, width, height, viewX, viewY, viewWidth, viewHeight,
            if (barHeight == 0) null else NavBar.render(viewWidth, barHeight),
        )
        relay = r
        val target = r.start()
        val cueSize = (viewHeight * CUE_FRACTION).roundToInt()
        backCue = Cues.back(cueSize)
        selectCue = Cues.select(cueSize)
        labelHeight = (viewHeight * LABEL_FRACTION).roundToInt()
        closeDisplay = Variant.openDisplay(context, viewWidth, viewHeight, density, target)
        current = this
        lastOutput = SystemClock.uptimeMillis()
        watchdogHandler.postDelayed(watchdog, WATCHDOG_INTERVAL_MS)
        watchdogHandler.postDelayed(preview, FIRST_PREVIEW_MS)
    }

    /** Answers the switchgear's back and select on the picture itself; see [Cues]. */
    fun cue(back: Boolean) {
        val badge = (if (back) backCue else selectCue) ?: return
        relay?.cue(badge)
    }

    private var labelHeight = 0

    /** [text] in a pill low on the picture, for a moment. */
    fun flash(text: String) {
        if (labelHeight == 0) return
        relay?.cue(Cues.label(text, labelHeight), FLASH_MS)
    }

    /** What the unit shows, now and then, as the on-connect picker's thumbnail. */
    private val preview = object : Runnable {
        override fun run() {
            val r = relay ?: return
            val app = Variant.setting(context, "app", "")
            r.capture { Previews.save(context, app, it) }
            watchdogHandler.postDelayed(this, PREVIEW_INTERVAL_MS)
        }
    }

    private fun intSetting(key: String, fallback: Int): Int =
        Variant.setting(context, key, fallback.toString()).toIntOrNull() ?: fallback

    /**
     * The relay redraws a still screen twice a second, so a long silence means the pipeline has
     * wedged; the session ends rather than leaving the unit on a frozen picture.
     */
    private val watchdog = object : Runnable {
        override fun run() {
            val silent = SystemClock.uptimeMillis() - lastOutput
            if (silent >= STALL_MS) {
                Log.w(TAG, "No frames from the encoder for ${silent}ms; ending the session")
                onError.run()
                return
            }
            watchdogHandler.postDelayed(this, WATCHDOG_INTERVAL_MS)
        }
    }

    fun requestKeyframe() {
        val c = codec ?: return
        try {
            c.setParameters(Bundle().apply { putInt(MediaCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0) })
        } catch (ignored: IllegalStateException) {
        }
    }

    fun stop() {
        if (current === this) current = null
        watchdogHandler.removeCallbacks(watchdog)
        watchdogHandler.removeCallbacks(preview)
        closeDisplay?.run()
        closeDisplay = null
        relay?.stop()
        relay = null
        val c = codec
        codec = null
        if (c != null) {
            try {
                c.stop()
            } catch (ignored: IllegalStateException) {
            }
            c.release()
        }
        thread.quitSafely()
    }

    /**
     * The start of the stream as the unit gets it, in files/stream-HHmmss.hevc (or .h264) beside
     * the log: playable, so what a unit shows black for can be looked at.
     */
    private fun sample(buffer: ByteBuffer, info: MediaCodec.BufferInfo) {
        try {
            var out = sample
            if (out == null) {
                val dir = context.getExternalFilesDir(null)
                if (dir == null) {
                    sampled = SAMPLE_BUFFERS
                    return
                }
                // One per session, by start time, the last few kept.
                val name = "stream-" + SimpleDateFormat("HHmmss", Locale.ROOT).format(Date()) +
                    if (codec?.name?.contains("hevc") == true) ".hevc" else ".h264"
                val old = dir.listFiles { _, n -> n.startsWith("stream-") }
                if (old != null && old.size >= KEPT_SAMPLES) {
                    old.sortBy { it.lastModified() }
                    for (i in 0..old.size - KEPT_SAMPLES) old[i].delete()
                }
                out = FileOutputStream(File(dir, name))
                sample = out
            }
            val bytes = ByteArray(info.size)
            buffer.position(info.offset)
            buffer.get(bytes)
            out.write(bytes)
            if (++sampled == SAMPLE_BUFFERS) out.close()
        } catch (e: IOException) {
            Log.w(TAG, "No stream sample", e)
            sampled = SAMPLE_BUFFERS
        }
    }

    private val callback = object : MediaCodec.Callback() {
        override fun onInputBufferAvailable(c: MediaCodec, index: Int) {}

        override fun onOutputBufferAvailable(c: MediaCodec, index: Int, info: MediaCodec.BufferInfo) {
            if (c !== codec) return
            lastOutput = SystemClock.uptimeMillis()
            try {
                val keyframe = info.flags and MediaCodec.BUFFER_FLAG_KEY_FRAME != 0
                if (keyframe) Log.i(TAG, "Keyframe of ${info.size} bytes")
                if (info.size > 0 && sampled < SAMPLE_BUFFERS) {
                    c.getOutputBuffer(index)?.let { sample(it, info) }
                }
                if (info.size > 0) {
                    c.getOutputBuffer(index)?.let {
                        Native.pushFrame(
                            session, it, info.offset, info.size, keyframe,
                            info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG != 0,
                        )
                    }
                }
                c.releaseOutputBuffer(index, false)
            } catch (ignored: IllegalStateException) {
            }
        }

        override fun onError(c: MediaCodec, e: MediaCodec.CodecException) {
            Log.e(TAG, "Encoder error", e)
            onError.run()
        }

        override fun onOutputFormatChanged(c: MediaCodec, format: MediaFormat) {
            Log.i(TAG, "Encoder output $format")
        }
    }
}
