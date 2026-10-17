package com.carjack;

import android.content.Context;
import android.content.Intent;
import android.hardware.display.VirtualDisplay;
import android.media.projection.MediaProjection;
import android.media.projection.MediaProjectionManager;
import android.os.Handler;
import android.os.Looper;
import android.util.Log;
import android.view.Surface;

/**
 * The user's screen-capture consent, and the one mirror display it may create: a projection makes
 * a single display, so later sessions resize and re-target it rather than make another.
 */
final class Projection {
    private static final String TAG = "carjack";

    private static MediaProjection projection;
    private static VirtualDisplay display;

    static synchronized void start(Context context, int resultCode, Intent data) {
        stop();
        projection = context.getSystemService(MediaProjectionManager.class)
                .getMediaProjection(resultCode, data);
        projection.registerCallback(new MediaProjection.Callback() {
            @Override
            public void onStop() {
                Log.i(TAG, "Screen capture stopped");
                synchronized (Projection.class) {
                    projection = null;
                    display = null;
                }
            }
        }, new Handler(Looper.getMainLooper()));
        Log.i(TAG, "Screen capture granted");
    }

    static synchronized VirtualDisplay display(int width, int height, int density,
            Surface target) {
        if (projection == null) {
            throw new IllegalStateException("No screen capture; start carjack from its app");
        }
        if (display == null) {
            display = projection.createVirtualDisplay("carjack", width, height, density, 0,
                    target, null, null);
        } else {
            display.resize(width, height, density);
            display.setSurface(target);
        }
        return display;
    }

    static synchronized void detach(VirtualDisplay d) {
        d.setSurface(null);
    }

    static synchronized void stop() {
        if (projection != null) {
            projection.stop();
        }
        projection = null;
        display = null;
    }

    private Projection() {}
}
