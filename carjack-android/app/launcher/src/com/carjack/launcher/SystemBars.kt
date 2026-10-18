package com.carjack.launcher

import android.app.Activity
import android.view.WindowInsets
import android.view.WindowInsetsController

/**
 * Keeps the status and navigation bars out of the way until a swipe in from their edge brings
 * them back.
 */
object SystemBars {
    fun hide(activity: Activity) {
        val controller = activity.window.insetsController ?: return
        // Transient: the swiped-in bars overlay the content and hide themselves again.
        controller.systemBarsBehavior = WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        controller.hide(WindowInsets.Type.systemBars())
    }
}
