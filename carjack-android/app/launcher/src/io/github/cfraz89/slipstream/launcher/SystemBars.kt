package io.github.cfraz89.slipstream.launcher

import android.app.Activity
import android.view.Display
import android.view.WindowInsets
import android.view.WindowInsetsController

/**
 * Keeps the status and navigation bars out of the way until a swipe in from their edge brings
 * them back - on the head unit. On the phone's own screen, opened from its app drawer, they stay.
 */
object SystemBars {
    /** Whether [activity] is on the phone's own screen rather than the head unit's. */
    fun onPhone(activity: Activity) = activity.display?.displayId == Display.DEFAULT_DISPLAY

    fun hide(activity: Activity) {
        if (onPhone(activity)) return
        val controller = activity.window.insetsController ?: return
        // Transient: the swiped-in bars overlay the content and hide themselves again.
        controller.systemBarsBehavior = WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        controller.hide(WindowInsets.Type.systemBars())
    }
}
