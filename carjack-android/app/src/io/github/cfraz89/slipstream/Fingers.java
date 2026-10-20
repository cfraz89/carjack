package io.github.cfraz89.slipstream;

import android.os.SystemClock;
import android.util.SparseArray;
import android.view.InputDevice;
import android.view.MotionEvent;

/**
 * The accessory's fingers, turned into Android's multi-pointer events: the first finger down is
 * ACTION_DOWN and the last up ACTION_UP, the others POINTER_DOWN/POINTER_UP, and every event
 * carries every finger that is down.
 */
final class Fingers {
    /** Each finger down, by pointer id, as {x, y}. */
    private final SparseArray<float[]> down = new SparseArray<>();
    private long downTime;

    /**
     * The event for {@code pointer} doing {@code action} at (x, y), in the display's pixels; null
     * for one that changes nothing, such as a move of a finger that isn't down. The caller sets
     * its display, injects it and recycles it.
     */
    MotionEvent event(int action, int pointer, float x, float y) {
        long now = SystemClock.uptimeMillis();
        int masked;
        switch (action) {
            case Touch.DOWN:
                if (down.get(pointer) != null) {
                    return event(Touch.MOVE, pointer, x, y);
                }
                down.put(pointer, new float[] {x, y});
                if (down.size() == 1) {
                    downTime = now;
                    masked = MotionEvent.ACTION_DOWN;
                } else {
                    masked = MotionEvent.ACTION_POINTER_DOWN;
                }
                break;
            case Touch.MOVE:
                if (down.get(pointer) == null) {
                    return null;
                }
                down.put(pointer, new float[] {x, y});
                masked = MotionEvent.ACTION_MOVE;
                break;
            case Touch.UP:
                if (down.get(pointer) == null) {
                    return null;
                }
                down.put(pointer, new float[] {x, y});
                masked = down.size() == 1 ? MotionEvent.ACTION_UP : MotionEvent.ACTION_POINTER_UP;
                break;
            default:
                return null;
        }

        int count = down.size();
        MotionEvent.PointerProperties[] props = new MotionEvent.PointerProperties[count];
        MotionEvent.PointerCoords[] coords = new MotionEvent.PointerCoords[count];
        for (int i = 0; i < count; i++) {
            props[i] = new MotionEvent.PointerProperties();
            props[i].id = down.keyAt(i);
            props[i].toolType = MotionEvent.TOOL_TYPE_FINGER;
            coords[i] = new MotionEvent.PointerCoords();
            coords[i].x = down.valueAt(i)[0];
            coords[i].y = down.valueAt(i)[1];
            coords[i].pressure = 1;
            coords[i].size = 1;
        }
        int index = down.indexOfKey(pointer);
        if (action == Touch.UP) {
            coords[index].pressure = 0;
            down.remove(pointer);
        }
        int full = masked == MotionEvent.ACTION_POINTER_DOWN
                || masked == MotionEvent.ACTION_POINTER_UP
                ? masked | index << MotionEvent.ACTION_POINTER_INDEX_SHIFT
                : masked;
        return MotionEvent.obtain(downTime, now, full, count, props, coords, 0, 0, 1, 1, 0, 0,
                InputDevice.SOURCE_TOUCHSCREEN, 0);
    }
}
