package com.carjack;

import android.hardware.input.InputManager;
import android.hardware.input.InputManagerGlobal;
import android.os.SystemClock;
import android.view.Display;
import android.view.InputDevice;
import android.view.MotionEvent;

/** The accessory's touches, from its panel's pixels onto the default display. */
final class TouchInjector {
    static final int DOWN = 0;
    static final int UP = 1;
    static final int MOVE = 2;

    private final int viewX;
    private final int viewY;
    private final int viewWidth;
    private final int viewHeight;
    private long downTime = -1;

    TouchInjector(int viewX, int viewY, int viewWidth, int viewHeight) {
        this.viewX = viewX;
        this.viewY = viewY;
        this.viewWidth = viewWidth;
        this.viewHeight = viewHeight;
    }

    void touch(int action, float panelX, float panelY) {
        float x = panelX - viewX;
        float y = panelY - viewY;
        long now = SystemClock.uptimeMillis();
        switch (action) {
            case DOWN:
                // A touch that lands on the accessory's own strip is not ours.
                if (x < 0 || y < 0 || x >= viewWidth || y >= viewHeight) {
                    return;
                }
                downTime = now;
                inject(MotionEvent.ACTION_DOWN, x, y, now);
                break;
            case MOVE:
                if (downTime >= 0) {
                    inject(MotionEvent.ACTION_MOVE, x, y, now);
                }
                break;
            case UP:
                if (downTime >= 0) {
                    inject(MotionEvent.ACTION_UP, x, y, now);
                    downTime = -1;
                }
                break;
        }
    }

    private void inject(int action, float x, float y, long now) {
        MotionEvent.PointerProperties props = new MotionEvent.PointerProperties();
        props.id = 0;
        props.toolType = MotionEvent.TOOL_TYPE_FINGER;
        MotionEvent.PointerCoords coords = new MotionEvent.PointerCoords();
        coords.x = Math.max(0, Math.min(viewWidth - 1, x));
        coords.y = Math.max(0, Math.min(viewHeight - 1, y));
        coords.pressure = action == MotionEvent.ACTION_UP ? 0 : 1;
        coords.size = 1;
        MotionEvent event = MotionEvent.obtain(downTime, now, action, 1,
                new MotionEvent.PointerProperties[] {props},
                new MotionEvent.PointerCoords[] {coords}, 0, 0, 1, 1, 0, 0,
                InputDevice.SOURCE_TOUCHSCREEN, 0);
        event.setDisplayId(Display.DEFAULT_DISPLAY);
        InputManagerGlobal.getInstance().injectInputEvent(event,
                InputManager.INJECT_INPUT_EVENT_MODE_ASYNC);
        event.recycle();
    }
}
