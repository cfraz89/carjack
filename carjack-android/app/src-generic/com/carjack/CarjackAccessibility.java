package com.carjack;

import android.accessibilityservice.AccessibilityService;
import android.accessibilityservice.GestureDescription;
import android.graphics.Path;
import android.os.Handler;
import android.os.Looper;
import android.os.SystemClock;
import android.view.accessibility.AccessibilityEvent;

/**
 * Touch control for the generic build: the only way an ordinary app can put touches on the screen.
 * A touch is one continued stroke, extended move by move, so a drag or a long press behaves as one.
 */
public final class CarjackAccessibility extends AccessibilityService {
    static volatile CarjackAccessibility instance;

    private final Handler handler = new Handler(Looper.getMainLooper());
    private GestureDescription.StrokeDescription stroke;
    private float lastX;
    private float lastY;
    private long lastTime;

    @Override
    protected void onServiceConnected() {
        instance = this;
    }

    @Override
    public boolean onUnbind(android.content.Intent intent) {
        instance = null;
        return super.onUnbind(intent);
    }

    @Override
    public void onAccessibilityEvent(AccessibilityEvent event) {}

    @Override
    public void onInterrupt() {}

    /** {@link Touch} actions, in screen pixels; from any thread. */
    void stroke(int action, float x, float y) {
        handler.post(() -> {
            long now = SystemClock.uptimeMillis();
            Path path = new Path();
            switch (action) {
                case Touch.DOWN:
                    path.moveTo(x, y);
                    stroke = new GestureDescription.StrokeDescription(path, 0, 1, true);
                    break;
                case Touch.MOVE:
                    if (stroke == null) {
                        return;
                    }
                    path.moveTo(lastX, lastY);
                    path.lineTo(x, y);
                    stroke = stroke.continueStroke(path, 0, Math.max(1, now - lastTime), true);
                    break;
                case Touch.UP:
                    if (stroke == null) {
                        return;
                    }
                    path.moveTo(lastX, lastY);
                    path.lineTo(x, y);
                    stroke = stroke.continueStroke(path, 0, Math.max(1, now - lastTime), false);
                    break;
                default:
                    return;
            }
            lastX = x;
            lastY = y;
            lastTime = now;
            dispatchGesture(new GestureDescription.Builder().addStroke(stroke).build(), null,
                    null);
            if (action == Touch.UP) {
                stroke = null;
            }
        });
    }
}
