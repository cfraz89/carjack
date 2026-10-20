package io.github.cfraz89.slipstream;

import java.nio.ByteBuffer;

/** The Rust core (carjack-android). */
final class Native {
    static {
        System.loadLibrary("carjack_android");
    }

    /** Called from the core's threads; none may block for long. */
    interface Callbacks {
        void joinWifi(String ssid, String passphrase, String bssid);

        void startVideo(int width, int height, int fps, int viewX, int viewY, int viewWidth,
                int viewHeight, boolean hevc);

        void requestKeyframe();

        /** {@code action}: 0 down, 1 up, 2 move, of finger {@code pointer}; in panel pixels. */
        void touch(int action, int pointer, float x, float y);

        /** {@code error} is null for a clean end. */
        void ended(String error);
    }

    static final int CODEC_AUTO = 0;
    static final int CODEC_H264 = 1;
    static final int CODEC_HEVC = 2;

    static native void init(String homekitPath);

    /** Takes ownership of {@code iapFd}. */
    static native long startSession(Callbacks callbacks, int iapFd, String name,
            String bluetoothAddress, int codec);

    static native void stopSession(long handle);

    /** {@code iface} null reports a failed join, with {@code detail} the reason; else the MAC. */
    static native void wifiJoined(long handle, String iface, String detail);

    static native void pushFrame(long handle, ByteBuffer buffer, int offset, int size,
            boolean keyframe, boolean codecConfig);

    private Native() {}
}
