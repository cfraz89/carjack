package io.github.cfraz89.slipstream

import android.content.Context
import android.os.Handler
import android.os.HandlerThread
import android.view.KeyEvent

/** The head unit's switchgear in Slipstream mode, as the profile's [Mappings] mapping maps it. */
object Switchgear {
    /** The head unit lets go of select within a few ms; held a moment, the press shows. */
    private const val SELECT_HOLD_MS = 150L

    /** Keys arrive on the core's threads: all of them here, in order. */
    private val handler = Handler(HandlerThread("slipstream-switchgear").apply { start() }.looper)
    /** Set on this thread before each press, for [presses] to act with. */
    private var context: Context? = null
    private val presses = Presses(handler) { action, down -> context?.let { perform(it, action, down) } }

    fun key(context: Context, code: Int, down: Boolean) {
        handler.post {
            this.context = context.applicationContext
            val input = Input.of(code)
            if (input == null) {
                Variant.key(context, code, down)
            } else {
                presses.press(Mappings.active(context), input, down)
            }
        }
    }

    /** Back or home from the phone remote's own buttons, not the switchgear's: as they are, unmapped. */
    fun navigate(context: Context, code: Int, down: Boolean) {
        handler.post { perform(context, if (code == KeyEvent.KEYCODE_HOME) Action.HOME else Action.BACK, down) }
    }

    private fun perform(context: Context, action: Action, down: Boolean) {
        when (action) {
            // Focus then visits everything on the screen in reading order, wrapping round, where
            // the arrows only reach what lies above or below.
            Action.NEXT -> Variant.inject(KeyEvent.KEYCODE_TAB, down, 0)
            Action.PREVIOUS -> Variant.inject(
                KeyEvent.KEYCODE_TAB, down, KeyEvent.META_SHIFT_ON or KeyEvent.META_SHIFT_LEFT_ON,
            )
            Action.UP -> Variant.key(context, KeyEvent.KEYCODE_DPAD_UP, down)
            Action.DOWN -> Variant.key(context, KeyEvent.KEYCODE_DPAD_DOWN, down)
            Action.LEFT -> Variant.key(context, KeyEvent.KEYCODE_DPAD_LEFT, down)
            Action.RIGHT -> Variant.key(context, KeyEvent.KEYCODE_DPAD_RIGHT, down)
            Action.SELECT -> select(context, down)
            Action.BACK -> {
                if (down) ScreenStream.current?.cue(true)
                Variant.key(context, KeyEvent.KEYCODE_BACK, down)
            }
            Action.HOME -> Variant.key(context, KeyEvent.KEYCODE_HOME, down)
            Action.VOICE -> Variant.key(context, KeyEvent.KEYCODE_VOICE_ASSIST, down)
            Action.NONE -> {}
        }
    }

    /** Sent whole at release, held long enough for the press to show. */
    private fun select(context: Context, down: Boolean) {
        if (down) return
        ScreenStream.current?.cue(false)
        Variant.key(context, KeyEvent.KEYCODE_DPAD_CENTER, true)
        handler.postDelayed({ Variant.key(context, KeyEvent.KEYCODE_DPAD_CENTER, false) }, SELECT_HOLD_MS)
    }
}
