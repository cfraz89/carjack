package io.github.cfraz89.slipstream

import android.os.Handler

/**
 * Presses of a switchgear input told apart where its mapping gives it a double or long press: a
 * single press then waits out [DOUBLE_MS] for a second, and each comes as a tap, down then up. An
 * input with neither acts as it's pressed, held for as long as it's held. Each call on [handler]'s
 * thread, and [perform] is called there.
 */
class Presses(private val handler: Handler, private val perform: (Action, Boolean) -> Unit) {
    private companion object {
        const val DOUBLE_MS = 300L
        const val LONG_MS = 600L
    }

    /** Released once, waiting for a second press. */
    private var waiting: Input? = null
    private var single: Runnable? = null
    private var long: Runnable? = null
    /** The held press already acted on, as a double or a long press: its release does nothing. */
    private var consumed = false

    fun press(mapping: Mapping, input: Input, down: Boolean) {
        val double = mapping.doublePress[input]
        val held = mapping.longPress[input]
        if (double == null && held == null) {
            perform(mapping.action(input), down)
            return
        }
        long?.let(handler::removeCallbacks)
        long = null
        if (down) {
            if (waiting == input && double != null) {
                single?.let(handler::removeCallbacks)
                single = null
                waiting = null
                consumed = true
                tap(double)
                return
            }
            // Another input's single press goes now rather than wait.
            single?.let {
                handler.removeCallbacks(it)
                it.run()
            }
            consumed = false
            if (held != null) {
                long = Runnable {
                    long = null
                    consumed = true
                    tap(held)
                }.also { handler.postDelayed(it, LONG_MS) }
            }
            return
        }
        if (consumed) return
        val action = mapping.action(input)
        if (double == null) {
            tap(action)
            return
        }
        waiting = input
        single = Runnable {
            single = null
            waiting = null
            tap(action)
        }.also { handler.postDelayed(it, DOUBLE_MS) }
    }

    private fun tap(action: Action) {
        perform(action, true)
        perform(action, false)
    }
}
