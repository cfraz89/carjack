package io.github.cfraz89.slipstream

import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager

/**
 * What the head unit gets, a setting: Slipstream mode, any of the phone's own apps, or Android Auto.
 * Both work with the phone locked and its screen off. Slipstream listens for the head unit either
 * way, from boot or the app's opening until it's turned off. A change takes effect mid-session.
 */
object SlipstreamMode {
    private const val PREF = "mode"
    /** As settings already hold it. */
    private const val SLIPSTREAM = "unlimited"
    private const val ANDROID_AUTO = "android_auto"

    /** Anything but Android Auto chosen outright, so unset or unknown values are Slipstream mode. */
    fun isOn(context: Context) = Variant.prefs(context).getString(PREF, SLIPSTREAM) != ANDROID_AUTO

    fun setOn(context: Context, on: Boolean) {
        Variant.prefs(context).edit().putString(PREF, if (on) SLIPSTREAM else ANDROID_AUTO).commit()
        Session.current?.layoutChanged()
    }

    /** The mode's name, as the settings and the phone remote show it. */
    fun name(context: Context) = if (isOn(context)) "Slipstream mode" else "Android Auto"

    /** The permissions a connected-device service can't start without: enough for Android Auto. */
    fun ready(context: Context) =
        SettingsActivity.PERMISSIONS.all { context.checkSelfPermission(it) == PackageManager.PERMISSION_GRANTED }

    /** Whether to listen; off from the notification's Turn off until the app is next used. */
    fun isListening(context: Context) = Profiles.global(context).getBoolean("listening", true)

    fun setListening(context: Context, on: Boolean) {
        Profiles.global(context).edit().putBoolean("listening", on).apply()
    }

    /** Listening for the head unit, where it's set up and hasn't been turned off. */
    fun listen(context: Context) {
        if (!ready(context) || !isListening(context)) return
        context.startForegroundService(Intent(context, SlipstreamService::class.java))
    }
}
