package io.github.cfraz89.slipstream;

import android.content.Context;
import android.media.MediaCodec;
import android.media.MediaCodecInfo;
import android.media.MediaCodecList;
import android.media.MediaFormat;
import android.os.Bundle;
import android.os.Handler;
import android.os.HandlerThread;
import android.util.Log;
import android.view.Surface;

import java.io.IOException;

/**
 * The display the {@link Variant} provides, sized to the part of the accessory's panel it gets,
 * into a hardware encoder whose output goes to the core.
 */
final class ScreenStream {
    private static final String TAG = "carjack";
    /** The UI's smallest width, in dp, unless the {@code density} setting says otherwise. */
    private static final int SMALLEST_WIDTH_DP = 480;
    private static final long REPEAT_FRAME_US = 100_000;
    private static final int DEFAULT_BITRATE = 8_000_000;
    /** What the Linux build gives the CFMOTO, which shows its HEVC: 3 Mbit/s VBR, IDR every 2 s. */
    private static final int HEVC_BITRATE = 3_000_000;
    private static final long STALL_MS = 10_000;
    private static final long WATCHDOG_INTERVAL_MS = 2_000;
    private static final long FIRST_PREVIEW_MS = 6_000;
    private static final long PREVIEW_INTERVAL_MS = 60_000;

    private final Context context;
    private final long session;
    private final Runnable onError;
    private final HandlerThread thread = new HandlerThread("carjack-encoder");
    private volatile MediaCodec codec;
    /** Closes the display the {@link Variant} opened. */
    private Runnable closeDisplay;
    private Relay relay;
    /** Off the encoder's thread, which is what the watchdog watches. */
    private final Handler watchdogHandler = new Handler(android.os.Looper.getMainLooper());
    private volatile long lastOutput;

    ScreenStream(Context context, long session, Runnable onError) {
        this.context = context;
        this.session = session;
        this.onError = onError;
    }

    /**
     * The panel is {@code width}x{@code height}; the display shows in the view rectangle, with
     * {@link NavBar} {@code barHeight} high under it (0 for none).
     */
    void start(int width, int height, int fps, int viewX, int viewY, int viewWidth,
            int viewHeight, int barHeight, boolean hevc) throws IOException {
        int density = intSetting("density",
                Math.max(120, Math.min(viewWidth, viewHeight) * 160 / SMALLEST_WIDTH_DP));

        String mime = hevc ? MediaFormat.MIMETYPE_VIDEO_HEVC : MediaFormat.MIMETYPE_VIDEO_AVC;
        MediaFormat format = MediaFormat.createVideoFormat(mime, width, height);
        format.setInteger(MediaFormat.KEY_COLOR_FORMAT,
                MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface);
        format.setInteger(MediaFormat.KEY_BIT_RATE,
                intSetting("bitrate", hevc ? HEVC_BITRATE : DEFAULT_BITRATE));
        format.setInteger(MediaFormat.KEY_FRAME_RATE, fps);
        format.setFloat(MediaFormat.KEY_MAX_FPS_TO_ENCODER, fps);
        format.setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, hevc ? 2 : 10);
        // A still screen still has to answer keyframe requests.
        format.setLong(MediaFormat.KEY_REPEAT_PREVIOUS_FRAME_AFTER, REPEAT_FRAME_US);
        format.setInteger(MediaFormat.KEY_PRIORITY, 0);

        thread.start();
        String encoder = pickEncoder(mime, width, height);
        codec = encoder != null
                ? MediaCodec.createByCodecName(encoder)
                : MediaCodec.createEncoderByType(mime);
        applyProfile(format, codec.getCodecInfo().getCapabilitiesForType(mime), hevc);
        Log.i(TAG, "Encoding " + width + "x" + height + "@" + fps + " with " + codec.getName()
                + "; display " + viewWidth + "x" + viewHeight + " at (" + viewX + "," + viewY
                + "), " + density + "dpi");
        codec.setCallback(callback, new Handler(thread.getLooper()));
        codec.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE);
        Surface input = codec.createInputSurface();
        codec.start();

        // Always through the relay: besides placing the view, it keeps a still screen sending.
        relay = new Relay(input, width, height, viewX, viewY, viewWidth, viewHeight,
                barHeight == 0 ? null : NavBar.render(viewWidth, barHeight));
        Surface target = relay.start();
        closeDisplay = Variant.openDisplay(context, viewWidth, viewHeight, density, target);
        lastOutput = android.os.SystemClock.uptimeMillis();
        watchdogHandler.postDelayed(watchdog, WATCHDOG_INTERVAL_MS);
        watchdogHandler.postDelayed(preview, FIRST_PREVIEW_MS);
    }

    /** What the unit shows, now and then, as the on-connect picker's thumbnail. */
    private final Runnable preview = new Runnable() {
        @Override
        public void run() {
            Relay r = relay;
            if (r == null) {
                return;
            }
            String app = Variant.setting(context, "app", "");
            r.capture(bitmap -> Previews.save(context, app, bitmap));
            watchdogHandler.postDelayed(this, PREVIEW_INTERVAL_MS);
        }
    };

    /**
     * Ask for what head units decode: HEVC Main at a variable bitrate, as the Linux build sends
     * it, and H.264 Constrained Baseline (else Baseline) at a constant one, rather than the High
     * profile phone encoders default to. A unit handed a profile it can't decode stops reading the
     * stream. Each only where the encoder offers it.
     */
    private static void applyProfile(MediaFormat format,
            MediaCodecInfo.CodecCapabilities caps, boolean hevc) {
        int[] wanted = hevc
                ? new int[] {MediaCodecInfo.CodecProfileLevel.HEVCProfileMain}
                : new int[] {MediaCodecInfo.CodecProfileLevel.AVCProfileConstrainedBaseline,
                        MediaCodecInfo.CodecProfileLevel.AVCProfileBaseline};
        pick:
        for (int profile : wanted) {
            for (MediaCodecInfo.CodecProfileLevel offered : caps.profileLevels) {
                if (offered.profile == profile) {
                    format.setInteger(MediaFormat.KEY_PROFILE, profile);
                    break pick;
                }
            }
        }
        int mode = hevc
                ? MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_VBR
                : MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_CBR;
        MediaCodecInfo.EncoderCapabilities encoder = caps.getEncoderCapabilities();
        if (encoder != null && encoder.isBitrateModeSupported(mode)) {
            format.setInteger(MediaFormat.KEY_BITRATE_MODE, mode);
        }
    }

    /**
     * A hardware encoder that takes this exact size, else any encoder that does; null leaves the
     * choice to the platform. A panel like 1080x600 isn't 16-line aligned, which some reject.
     */
    private static String pickEncoder(String mime, int width, int height) {
        String fallback = null;
        for (MediaCodecInfo info : new MediaCodecList(MediaCodecList.REGULAR_CODECS)
                .getCodecInfos()) {
            if (!info.isEncoder()) {
                continue;
            }
            for (String type : info.getSupportedTypes()) {
                if (!type.equalsIgnoreCase(mime)) {
                    continue;
                }
                MediaCodecInfo.VideoCapabilities video =
                        info.getCapabilitiesForType(type).getVideoCapabilities();
                if (video == null || !video.isSizeSupported(width, height)) {
                    continue;
                }
                if (info.isHardwareAccelerated()) {
                    return info.getName();
                }
                if (fallback == null) {
                    fallback = info.getName();
                }
            }
        }
        if (fallback == null) {
            Log.w(TAG, "No " + mime + " encoder claims " + width + "x" + height);
        }
        return fallback;
    }

    private int intSetting(String key, int fallback) {
        try {
            return Integer.parseInt(Variant.setting(context, key, Integer.toString(fallback)));
        } catch (NumberFormatException e) {
            return fallback;
        }
    }

    /**
     * The relay redraws a still screen twice a second, so a long silence means the pipeline has
     * wedged; the session ends rather than leaving the unit on a frozen picture.
     */
    private final Runnable watchdog = new Runnable() {
        @Override
        public void run() {
            long silent = android.os.SystemClock.uptimeMillis() - lastOutput;
            if (silent >= STALL_MS) {
                Log.w(TAG, "No frames from the encoder for " + silent + "ms; ending the session");
                onError.run();
                return;
            }
            watchdogHandler.postDelayed(this, WATCHDOG_INTERVAL_MS);
        }
    };

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
        watchdogHandler.removeCallbacks(watchdog);
        watchdogHandler.removeCallbacks(preview);
        if (closeDisplay != null) {
            closeDisplay.run();
            closeDisplay = null;
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

    /** How much of each session's start is kept, in encoder buffers: the config and ~6 s. */
    private static final int SAMPLE_BUFFERS = 60;
    private static final int KEPT_SAMPLES = 6;
    private int sampled;
    private java.io.FileOutputStream sample;

    /**
     * The start of the stream as the unit gets it, in files/stream-HHmmss.hevc (or .h264) beside
     * the log: playable, so what a unit shows black for can be looked at.
     */
    private void sample(java.nio.ByteBuffer buffer, MediaCodec.BufferInfo info) {
        try {
            if (sample == null) {
                java.io.File dir = context.getExternalFilesDir(null);
                if (dir == null) {
                    sampled = SAMPLE_BUFFERS;
                    return;
                }
                // One per session, by start time, the last few kept.
                String name = "stream-" + new java.text.SimpleDateFormat("HHmmss",
                        java.util.Locale.ROOT).format(new java.util.Date())
                        + (codec.getName().contains("hevc") ? ".hevc" : ".h264");
                java.io.File[] old = dir.listFiles((d, n) -> n.startsWith("stream-"));
                if (old != null && old.length >= KEPT_SAMPLES) {
                    java.util.Arrays.sort(old, (a, b) -> Long.compare(a.lastModified(),
                            b.lastModified()));
                    for (int i = 0; i <= old.length - KEPT_SAMPLES; i++) {
                        old[i].delete();
                    }
                }
                sample = new java.io.FileOutputStream(new java.io.File(dir, name));
            }
            byte[] bytes = new byte[info.size];
            buffer.position(info.offset);
            buffer.get(bytes);
            sample.write(bytes);
            if (++sampled == SAMPLE_BUFFERS) {
                sample.close();
            }
        } catch (IOException e) {
            Log.w(TAG, "No stream sample", e);
            sampled = SAMPLE_BUFFERS;
        }
    }

    private final MediaCodec.Callback callback = new MediaCodec.Callback() {
        @Override
        public void onInputBufferAvailable(MediaCodec c, int index) {}

        @Override
        public void onOutputBufferAvailable(MediaCodec c, int index, MediaCodec.BufferInfo info) {
            if (c != codec) {
                return;
            }
            lastOutput = android.os.SystemClock.uptimeMillis();
            try {
                if ((info.flags & MediaCodec.BUFFER_FLAG_KEY_FRAME) != 0) {
                    Log.i(TAG, "Keyframe of " + info.size + " bytes");
                }
                if (info.size > 0 && sampled < SAMPLE_BUFFERS) {
                    sample(c.getOutputBuffer(index), info);
                }
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
