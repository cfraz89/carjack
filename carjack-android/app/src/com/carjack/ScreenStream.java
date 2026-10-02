package com.carjack;

import android.content.Context;
import android.hardware.display.DisplayManager;
import android.hardware.display.VirtualDisplay;
import android.media.MediaCodec;
import android.media.MediaCodecInfo;
import android.media.MediaFormat;
import android.os.Bundle;
import android.os.Handler;
import android.os.HandlerThread;
import android.os.SystemProperties;
import android.os.UserHandle;
import android.util.Log;
import android.view.Display;
import android.view.IWindowManager;
import android.view.Surface;
import android.view.WindowManagerGlobal;

import java.io.IOException;

/**
 * The default display, sized to the part of the accessory's panel it gets, mirrored into a
 * hardware encoder whose output goes to the core.
 */
final class ScreenStream {
    private static final String TAG = "carjack";
    /** The UI's smallest width, in dp, unless {@code persist.carjack.density} says otherwise. */
    private static final int SMALLEST_WIDTH_DP = 480;
    private static final long REPEAT_FRAME_US = 100_000;
    private static final int DEFAULT_BITRATE = 8_000_000;

    private final Context context;
    private final long session;
    private final Runnable onError;
    private final HandlerThread thread = new HandlerThread("carjack-encoder");
    private volatile MediaCodec codec;
    private VirtualDisplay display;
    private Relay relay;

    ScreenStream(Context context, long session, Runnable onError) {
        this.context = context;
        this.session = session;
        this.onError = onError;
    }

    /** The panel is {@code width}x{@code height}; the display shows in the view rectangle. */
    void start(int width, int height, int fps, int viewX, int viewY, int viewWidth,
            int viewHeight, boolean hevc) throws IOException {
        int density = SystemProperties.getInt("persist.carjack.density",
                Math.max(120, Math.min(viewWidth, viewHeight) * 160 / SMALLEST_WIDTH_DP));
        resizeDefaultDisplay(viewWidth, viewHeight, density);

        String mime = hevc ? MediaFormat.MIMETYPE_VIDEO_HEVC : MediaFormat.MIMETYPE_VIDEO_AVC;
        MediaFormat format = MediaFormat.createVideoFormat(mime, width, height);
        format.setInteger(MediaFormat.KEY_COLOR_FORMAT,
                MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface);
        format.setInteger(MediaFormat.KEY_BIT_RATE,
                SystemProperties.getInt("persist.carjack.bitrate", DEFAULT_BITRATE));
        format.setInteger(MediaFormat.KEY_FRAME_RATE, fps);
        format.setFloat(MediaFormat.KEY_MAX_FPS_TO_ENCODER, fps);
        format.setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 10);
        // A still screen still has to answer keyframe requests.
        format.setLong(MediaFormat.KEY_REPEAT_PREVIOUS_FRAME_AFTER, REPEAT_FRAME_US);
        format.setInteger(MediaFormat.KEY_PRIORITY, 0);
        if (hevc) {
            format.setInteger(MediaFormat.KEY_PROFILE,
                    MediaCodecInfo.CodecProfileLevel.HEVCProfileMain);
        }

        thread.start();
        codec = MediaCodec.createEncoderByType(mime);
        Log.i(TAG, "Encoding " + width + "x" + height + "@" + fps + " with " + codec.getName()
                + "; display " + viewWidth + "x" + viewHeight + " at (" + viewX + "," + viewY
                + "), " + density + "dpi");
        codec.setCallback(callback, new Handler(thread.getLooper()));
        codec.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE);
        Surface input = codec.createInputSurface();
        codec.start();

        Surface target = input;
        boolean fills = viewX == 0 && viewY == 0 && viewWidth == width && viewHeight == height;
        if (!fills) {
            relay = new Relay(input, width, height, viewX, viewY, viewWidth, viewHeight);
            target = relay.start();
        }
        display = context.getSystemService(DisplayManager.class).createVirtualDisplay(
                "carjack", viewWidth, viewHeight, density, target,
                DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR
                        | DisplayManager.VIRTUAL_DISPLAY_FLAG_SECURE);
    }

    private static void resizeDefaultDisplay(int width, int height, int density) {
        try {
            IWindowManager wm = WindowManagerGlobal.getWindowManagerService();
            wm.setForcedDisplaySize(Display.DEFAULT_DISPLAY, width, height);
            wm.setForcedDisplayDensityForUser(Display.DEFAULT_DISPLAY, density,
                    UserHandle.USER_CURRENT);
        } catch (Exception e) {
            Log.w(TAG, "Could not resize the display", e);
        }
    }

    void requestKeyframe() {
        MediaCodec c = codec;
        if (c == null) {
            return;
        }
        Bundle params = new Bundle();
        params.putInt(MediaCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0);
        try {
            c.setParameters(params);
        } catch (IllegalStateException ignored) {
        }
    }

    void stop() {
        if (display != null) {
            display.release();
            display = null;
        }
        if (relay != null) {
            relay.stop();
            relay = null;
        }
        MediaCodec c = codec;
        codec = null;
        if (c != null) {
            try {
                c.stop();
            } catch (IllegalStateException ignored) {
            }
            c.release();
        }
        thread.quitSafely();
    }

    private final MediaCodec.Callback callback = new MediaCodec.Callback() {
        @Override
        public void onInputBufferAvailable(MediaCodec c, int index) {}

        @Override
        public void onOutputBufferAvailable(MediaCodec c, int index, MediaCodec.BufferInfo info) {
            if (c != codec) {
                return;
            }
            try {
                if (info.size > 0) {
                    Native.pushFrame(session, c.getOutputBuffer(index), info.offset, info.size,
                            (info.flags & MediaCodec.BUFFER_FLAG_KEY_FRAME) != 0,
                            (info.flags & MediaCodec.BUFFER_FLAG_CODEC_CONFIG) != 0);
                }
                c.releaseOutputBuffer(index, false);
            } catch (IllegalStateException ignored) {
            }
        }

        @Override
        public void onError(MediaCodec c, MediaCodec.CodecException e) {
            Log.e(TAG, "Encoder error", e);
            onError.run();
        }

        @Override
        public void onOutputFormatChanged(MediaCodec c, MediaFormat format) {
            Log.i(TAG, "Encoder output " + format);
        }
    };
}
