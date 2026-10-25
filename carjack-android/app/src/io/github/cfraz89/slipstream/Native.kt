package io.github.cfraz89.slipstream

import java.nio.ByteBuffer

/**
 * The Rust core (carjack-android). Its JNI symbols name this class and these methods, so they
 * stay static ([JvmStatic]) with these signatures.
 */
object Native {
    init {
        System.loadLibrary("carjack_android")
    }

    /** Called from the core's threads; none may block for long. */
    interface Callbacks {
        fun joinWifi(ssid: String, passphrase: String?, bssid: String?)

        fun startVideo(
            width: Int, height: Int, fps: Int, viewX: Int, viewY: Int, viewWidth: Int,
            viewHeight: Int, hevc: Boolean,
        )

        fun requestKeyframe()

        /** [action]: 0 down, 1 up, 2 move, of finger [pointer]; in panel pixels. */
        fun touch(action: Int, pointer: Int, x: Float, y: Float)

        /** A key from the accessory's switchgear: an Android key code, going down or up. */
        fun key(code: Int, down: Boolean)

        /** [error] is null for a clean end. */
        fun ended(error: String?)
    }

    /** The core's log lines of note, from any of its threads; must not block. */
    fun interface Reporter {
        /** [priority] as logcat's: 4 info, 5 warning, 6 error, 7 a panic. */
        fun report(priority: Int, target: String, message: String)
    }

    const val CODEC_AUTO = 0
    const val CODEC_H264 = 1
    const val CODEC_HEVC = 2

    @JvmStatic
    external fun init(homekitPath: String, reporter: Reporter?)

    /** Takes ownership of [iapFd]. */
    @JvmStatic
    external fun startSession(
        callbacks: Callbacks, iapFd: Int, name: String?, bluetoothAddress: String?, codec: Int,
    ): Long

    @JvmStatic
    external fun stopSession(handle: Long)

    /** [iface] null reports a failed join, with [detail] the reason; else the MAC. */
    @JvmStatic
    external fun wifiJoined(handle: Long, iface: String?, detail: String?)

    @JvmStatic
    external fun pushFrame(
        handle: Long, buffer: ByteBuffer, offset: Int, size: Int, keyframe: Boolean,
        codecConfig: Boolean,
    )
}
