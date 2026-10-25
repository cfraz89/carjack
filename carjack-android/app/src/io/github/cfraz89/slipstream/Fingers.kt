package io.github.cfraz89.slipstream

import android.os.SystemClock
import android.util.SparseArray
import android.view.InputDevice
import android.view.MotionEvent

/**
 * The accessory's fingers, turned into Android's multi-pointer events: the first finger down is
 * ACTION_DOWN and the last up ACTION_UP, the others POINTER_DOWN/POINTER_UP, and every event
 * carries every finger that is down.
 */
class Fingers {
    /** Each finger down, by pointer id, as {x, y}. */
    private val down = SparseArray<FloatArray>()
    private var downTime = 0L

    /**
     * The event for [pointer] doing [action] at (x, y), in the display's pixels; null for one
     * that changes nothing, such as a move of a finger that isn't down. The caller sets its
     * display, injects it and recycles it.
     */
    fun event(action: Int, pointer: Int, x: Float, y: Float): MotionEvent? {
        val now = SystemClock.uptimeMillis()
        val masked = when (action) {
            Touch.DOWN -> {
                if (down.get(pointer) != null) return event(Touch.MOVE, pointer, x, y)
                down.put(pointer, floatArrayOf(x, y))
                if (down.size() == 1) {
                    downTime = now
                    MotionEvent.ACTION_DOWN
                } else {
                    MotionEvent.ACTION_POINTER_DOWN
                }
            }
            Touch.MOVE -> {
                if (down.get(pointer) == null) return null
                down.put(pointer, floatArrayOf(x, y))
                MotionEvent.ACTION_MOVE
            }
            Touch.UP -> {
                if (down.get(pointer) == null) return null
                down.put(pointer, floatArrayOf(x, y))
                if (down.size() == 1) MotionEvent.ACTION_UP else MotionEvent.ACTION_POINTER_UP
            }
            else -> return null
        }

        val count = down.size()
        val props = Array(count) { i ->
            MotionEvent.PointerProperties().apply {
                id = down.keyAt(i)
                toolType = MotionEvent.TOOL_TYPE_FINGER
            }
        }
        val coords = Array(count) { i ->
            MotionEvent.PointerCoords().apply {
                this.x = down.valueAt(i)[0]
                this.y = down.valueAt(i)[1]
                pressure = 1f
                size = 1f
            }
        }
        val index = down.indexOfKey(pointer)
        if (action == Touch.UP) {
            coords[index].pressure = 0f
            down.remove(pointer)
        }
        val full = if (masked == MotionEvent.ACTION_POINTER_DOWN ||
            masked == MotionEvent.ACTION_POINTER_UP
        ) {
            masked or (index shl MotionEvent.ACTION_POINTER_INDEX_SHIFT)
        } else {
            masked
        }
        return MotionEvent.obtain(
            downTime, now, full, count, props, coords, 0, 0, 1f, 1f, 0, 0,
            InputDevice.SOURCE_TOUCHSCREEN, 0,
        )
    }
}
