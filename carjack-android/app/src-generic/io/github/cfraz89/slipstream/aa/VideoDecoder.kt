// Adapted from OpenCfMoto and headunit-revived (AGPL-3.0): decoder/VideoDecoder.kt, aap/AapVideo.kt.
package io.github.cfraz89.slipstream.aa

import android.media.MediaCodec
import android.media.MediaCodecList
import android.media.MediaFormat
import android.os.SystemClock
import android.view.Surface
import java.nio.ByteBuffer

/**
 * Android Auto's H.264, reassembled from the video channel's fragments and decoded onto [surface],
 * at the size Android Auto was asked for.
 */
class VideoDecoder(private val surface: Surface, private val width: Int, private val height: Int) {
    private companion object {
        const val TIMEOUT_US = 10_000L
        /** The video channel's fragment flags, on top of encrypted: only, first, middle, last. */
        const val ONLY = 11
        const val FIRST = 9
        const val MIDDLE = 8
        const val LAST = 10
        const val STALL_MS = 3_000L
    }

    private val assembly = ByteBuffer.allocate(AapMessage.BUFFER_LENGTH * 16)
    private var corrupt = false
    private var codec: MediaCodec? = null
    private var output: Thread? = null
    @Volatile private var running = false
    @Volatile private var lastInputMs = 0L
    private var startNs = 0L

    /** Asked when the stream needs a fresh keyframe: after a lost fragment, or a stall. */
    var onNeedKeyframe: (() -> Unit)? = null

    /** A video channel message; false for one that isn't video data. */
    fun process(message: AapMessage): Boolean {
        val buf = message.data
        val len = message.size
        when (message.flags.toInt()) {
            ONLY -> {
                corrupt = false
                assembly.clear()
                val start = payloadStart(buf, len) ?: return false
                decode(buf, start, len - start)
            }
            FIRST -> {
                corrupt = false
                assembly.clear()
                val start = payloadStart(buf, len) ?: return false
                assembly.put(buf, start, len - start)
            }
            MIDDLE, LAST -> {
                if (corrupt) return true
                if (assembly.remaining() < len) {
                    AaLog.w("Video fragment overflow; waiting for a keyframe")
                    corrupt = true
                    assembly.clear()
                    onNeedKeyframe?.invoke()
                    return true
                }
                assembly.put(buf, 0, len)
                if (message.flags.toInt() == LAST) {
                    decode(assembly.array(), 0, assembly.position())
                    assembly.clear()
                }
            }
            else -> return false
        }
        return true
    }

    /**
     * Where the H.264 starts: past the type and a timestamp (10 bytes) for data, past the type
     * alone (2) for codec config. Null for a message that's neither.
     */
    private fun payloadStart(buf: ByteArray, len: Int): Int? {
        for (start in intArrayOf(10, 2)) {
            if (len > start + 4 && buf[start].toInt() == 0 && buf[start + 1].toInt() == 0 &&
                (buf[start + 2].toInt() == 1 || (buf[start + 2].toInt() == 0 && buf[start + 3].toInt() == 1))
            ) {
                return start
            }
        }
        return null
    }

    @Synchronized
    private fun decode(buf: ByteArray, offset: Int, size: Int) {
        val c = codec ?: start() ?: return
        try {
            val index = c.dequeueInputBuffer(TIMEOUT_US * 30)
            if (index < 0) {
                AaLog.w("Decoder input full; frame dropped")
                return
            }
            val input = c.getInputBuffer(index) ?: return
            input.clear()
            val n = minOf(size, input.capacity())
            input.put(buf, offset, n)
            val config = isConfig(buf, offset, size)
            c.queueInputBuffer(
                index, 0, n, (System.nanoTime() - startNs) / 1000,
                if (config) MediaCodec.BUFFER_FLAG_CODEC_CONFIG else 0,
            )
            lastInputMs = SystemClock.elapsedRealtime()
        } catch (e: IllegalStateException) {
            AaLog.e("Decoder input failed", e)
            restart()
        }
    }

    /** SPS or PPS first: codec config. */
    private fun isConfig(buf: ByteArray, offset: Int, size: Int): Boolean {
        val nal = if (size > 4 && buf[offset + 2].toInt() == 1) offset + 3 else offset + 4
        if (nal >= offset + size) return false
        val type = buf[nal].toInt() and 0x1F
        return type == 7 || type == 8
    }

    private fun start(): MediaCodec? {
        if (!surface.isValid) return null
        val name = MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos
            .filter { !it.isEncoder && it.supportedTypes.any { t -> t.equals(MediaFormat.MIMETYPE_VIDEO_AVC, true) } }
            .sortedBy { if (it.isHardwareAccelerated) 0 else 1 }
            .firstOrNull()?.name
        return try {
            val c = if (name != null) {
                MediaCodec.createByCodecName(name)
            } else {
                MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_VIDEO_AVC)
            }
            val format = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, width, height)
            format.setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, 2 * 1024 * 1024)
            // Otherwise a stream that doesn't bound its reordering is held back a dozen frames or so.
            format.setInteger(MediaFormat.KEY_LOW_LATENCY, 1)
            format.setInteger(MediaFormat.KEY_PRIORITY, 0)
            c.configure(format, surface, null, 0)
            c.setVideoScalingMode(MediaCodec.VIDEO_SCALING_MODE_SCALE_TO_FIT)
            c.start()
            AaLog.i("Decoding ${width}x$height with ${c.name}")
            codec = c
            startNs = System.nanoTime()
            running = true
            output = Thread(::drain, "carjack-aa-decoder").apply { start() }
            c
        } catch (e: Exception) {
            AaLog.e("Could not start the decoder", e)
            null
        }
    }

    /** Renders each decoded frame; a decoder taking input but giving nothing back is restarted. */
    private fun drain() {
        val info = MediaCodec.BufferInfo()
        var lastOutputMs = SystemClock.elapsedRealtime()
        while (running) {
            val c = codec ?: break
            try {
                val index = c.dequeueOutputBuffer(info, TIMEOUT_US)
                if (index >= 0) {
                    // On the relay's clock: a frame stamped by the stream's own runs behind the
                    // relay's redraws, and the encoder drops it.
                    c.releaseOutputBuffer(index, System.nanoTime())
                    lastOutputMs = SystemClock.elapsedRealtime()
                }
            } catch (e: IllegalStateException) {
                if (running) AaLog.w("Decoder output failed: ${e.message}")
                break
            }
            val now = SystemClock.elapsedRealtime()
            if (now - lastOutputMs > STALL_MS && now - lastInputMs < 1000) {
                AaLog.w("Decoder stalled; restarting")
                Thread { restart() }.start()
                break
            }
        }
    }

    @Synchronized
    private fun restart() {
        stop()
        onNeedKeyframe?.invoke()
    }

    @Synchronized
    fun stop() {
        running = false
        val t = output
        output = null
        if (t != null && t !== Thread.currentThread()) t.join(500)
        codec?.let {
            try {
                it.stop()
            } catch (ignored: IllegalStateException) {
            }
            it.release()
        }
        codec = null
    }
}
