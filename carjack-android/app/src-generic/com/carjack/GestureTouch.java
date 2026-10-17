package com.carjack;

import android.content.Context;
import android.hardware.display.DisplayManager;
import android.util.DisplayMetrics;
import android.util.Log;
import android.view.Display;

/**
 * The accessory's touches onto the phone's screen. The mirror shows the whole screen scaled to fit
 * the view and centred, so a touch is mapped back through that, at the screen's current rotation,
 * and replayed by {@link CarjackAccessibility}.
 */
final class GestureTouch implements Touch {
    private static final String TAG = "carjack";

    private final Context context;
    private final int viewX;
    private final int viewY;
    private final int viewWidth;
    private final int viewHeight;
    private boolean down;
    private boolean warned;

    GestureTouch(Context context, int viewX, int viewY, int viewWidth, int viewHeight) {
        this.context = context;
        this.viewX = viewX;
        this.viewY = viewY;
        this.viewWidth = viewWidth;
        this.viewHeight = viewHeight;
    }

    @Override
    public void touch(int action, float panelX, float panelY) {
        CarjackAccessibility service = CarjackAccessibility.instance;
        if (service == null) {
            if (!warned) {
                Log.w(TAG, "Touch control is off; enable carjack under Accessibility");
                warned = true;
            }
            return;
        }
        DisplayMetrics screen = new DisplayMetrics();
        context.getSystemService(DisplayManager.class).getDisplay(Display.DEFAULT_DISPLAY)
                .getRealMetrics(screen);
        float scale = Math.min((float) viewWidth / screen.widthPixels,
                (float) viewHeight / screen.heightPixels);
        float left = (viewWidth - screen.widthPixels * scale) / 2;
        float top = (viewHeight - screen.heightPixels * scale) / 2;
        float x = (panelX - viewX - left) / scale;
        float y = (panelY - viewY - top) / scale;
        boolean inside = x >= 0 && y >= 0 && x < screen.widthPixels && y < screen.heightPixels;
        x = Math.max(0, Math.min(screen.widthPixels - 1, x));
        y = Math.max(0, Math.min(screen.heightPixels - 1, y));

        switch (action) {
            case DOWN:
                // A touch on the bars around the picture is not on the screen.
                if (!inside) {
                    return;
                }
                down = true;
                service.stroke(action, x, y);
                break;
            case MOVE:
                if (down) {
                    service.stroke(action, x, y);
                }
                break;
            case UP:
                if (down) {
                    service.stroke(action, x, y);
                    down = false;
                }
                break;
        }
    }
}
