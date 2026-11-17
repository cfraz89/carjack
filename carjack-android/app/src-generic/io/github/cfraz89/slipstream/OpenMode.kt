package io.github.cfraz89.slipstream

import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager

/**
 * What the head unit gets, a setting: Android Auto, or Open mode, the phone's own apps. Both work
 * with the phone locked and its screen off. Slipstream listens for the head unit either way, from
 * boot or the app's opening until it's turned off. A change takes effect mid-session.
 */
object OpenMode {
    fun isOn(context: Context) = Variant.prefs(context).getString("mode", "open") == "open"

    fun setOn(context: Context, on: Boolean) {
        Variant.prefs(context).edit().putString("mode", if (on) "open" else "android_auto").apply()
        ScreenStream.current?.reopen()
    }

    /** The permissions a connected-device service can't start without: enough for Android Auto. */
    fun ready(context: Context) =
        SettingsActivity.PERMISSIONS.all { context.checkSelfPermission(it) == PackageManager.PERMISSION_GRANTED }

    /** Whether to listen; off from the notification's Turn off until the app is next used. */
    fun isListening(context: Context) = Variant.prefs(context).getBoolean("listening", true)

    fun setListening(context: Context, on: Boolean) {
        Variant.prefs(context).edit().putBoolean("listening", on).apply()
    }

    /** Listening for the head unit, where it's set up and hasn't been turned off. */
    fun listen(context: Context) {
        if (!ready(context) || !isListening(context)) return
        context.startForegroundService(Intent(context, SlipstreamService::class.java))
    }
}
