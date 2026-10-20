package io.github.cfraz89.slipstream;

import android.hardware.input.InputManager;
import android.hardware.input.InputManagerGlobal;
import android.view.Display;
import android.view.MotionEvent;

/** The accessory's touches, from its panel's pixels onto the default display. */
final class TouchInjector implements Touch {
    private final int viewX;
    private final int viewY;
    private final int viewWidth;
    private final int viewHeight;
    private final Fingers fingers = new Fingers();

    TouchInjector(int viewX, int viewY, int viewWidth, int viewHeight) {
        this.viewX = viewX;
        this.viewY = viewY;
        this.viewWidth = viewWidth;
        this.viewHeight = viewHeight;
    }

    @Override
    public void touch(int action, int pointer, float panelX, float panelY) {
        float x = panelX - viewX;
        float y = panelY - viewY;
        // A finger that lands on the accessory's own strip is not ours, nor what it does next.
        if (action == DOWN && (x < 0 || y < 0 || x >= viewWidth || y >= viewHeight)) {
            return;
        }
        MotionEvent event = fingers.event(action, pointer,
                Math.max(0, Math.min(viewWidth - 1, x)),
                Math.max(0, Math.min(viewHeight - 1, y)));
        if (event == null) {
            return;
        }
        event.setDisplayId(Display.DEFAULT_DISPLAY);
        InputManagerGlobal.getInstance().injectInputEvent(event,
                InputManager.INJECT_INPUT_EVENT_MODE_ASYNC);
        event.recycle();
    }
}
