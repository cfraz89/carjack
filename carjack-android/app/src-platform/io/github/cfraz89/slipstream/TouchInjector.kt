package io.github.cfraz89.slipstream

import android.hardware.input.InputManager
import android.hardware.input.InputManagerGlobal
import android.view.Display

/** The accessory's touches, from its panel's pixels onto the default display. */
class TouchInjector(
    private val viewX: Int,
    private val viewY: Int,
    private val viewWidth: Int,
    private val viewHeight: Int,
) : Touch {
    private val fingers = Fingers()

    override fun touch(action: Int, pointer: Int, panelX: Float, panelY: Float) {
        val x = panelX - viewX
        val y = panelY - viewY
        // A finger that lands on the accessory's own strip is not ours, nor what it does next.
        if (action == Touch.DOWN && (x < 0 || y < 0 || x >= viewWidth || y >= viewHeight)) return
        val event = fingers.event(
            action, pointer,
            x.coerceIn(0f, (viewWidth - 1).toFloat()),
            y.coerceIn(0f, (viewHeight - 1).toFloat()),
        ) ?: return
        event.setDisplayId(Display.DEFAULT_DISPLAY)
        InputManagerGlobal.getInstance()
            .injectInputEvent(event, InputManager.INJECT_INPUT_EVENT_MODE_ASYNC)
        event.recycle()
    }
}
