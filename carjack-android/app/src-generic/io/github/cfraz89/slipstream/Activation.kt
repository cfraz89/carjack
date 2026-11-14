package io.github.cfraz89.slipstream

import android.app.Activity
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager

/**
 * Slipstream runs only while activated: started by hand, with the phone unlocked, and kept to the
 * dashboard, which keeps the screen on - and so unlocked - across head unit sessions, until it's
 * ended by hand. Nothing activates it by itself: a locked phone gives the head unit a blank display.
 */
object Activation {
    fun isOn(context: Context) = Variant.prefs(context).getBoolean("on", false)

    fun setOn(context: Context, on: Boolean) {
        Variant.prefs(context).edit().putBoolean("on", on).apply()
    }

    /** Shizuku, and the permissions a connected-device service can't start without. */
    fun ready(context: Context) = ShizukuDisplay.available() &&
        SettingsActivity.PERMISSIONS.all { context.checkSelfPermission(it) == PackageManager.PERMISSION_GRANTED }

    /** The service alone, for a caller that opens the dashboard itself. */
    fun startService(context: Context) {
        context.startForegroundService(Intent(context, SlipstreamService::class.java))
        setOn(context, true)
    }

    fun start(context: Context) {
        startService(context)
        context.startActivity(dashboard(context))
    }

    fun stop(context: Context) {
        setOn(context, false)
        context.stopService(Intent(context, SlipstreamService::class.java))
    }

    /** In the app's one task: from an activity that's the caller's, which then gives way to it. */
    fun dashboard(context: Context): Intent = Intent(context, DashboardActivity::class.java).apply {
        if (context !is Activity) addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
    }
}
