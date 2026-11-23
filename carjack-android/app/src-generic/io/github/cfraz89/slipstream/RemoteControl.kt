package io.github.cfraz89.slipstream

import android.content.Context

/**
 * The controls the phone remote shows, each a setting. The keys send what the head unit's
 * switchgear would.
 */
enum class RemoteControl(private val pref: String, val title: String, val icon: Int, private val byDefault: Boolean) {
    /** The launcher's shortcuts and all apps, opened on the head unit: Slipstream mode only. */
    APPS("remote_apps", "Apps", R.drawable.ic_sl_apps, true),
    DPAD("remote_dpad", "D-pad", R.drawable.ic_sl_dpad, true),
    MEDIA("remote_media", "Media buttons", R.drawable.ic_sl_music, true),
    NAV("remote_nav", "Back and home", R.drawable.ic_sl_home, true);

    fun shown(context: Context) = Variant.prefs(context).getBoolean(pref, byDefault)

    fun setShown(context: Context, on: Boolean) {
        Variant.prefs(context).edit().putBoolean(pref, on).apply()
    }

    companion object {
        fun shown(context: Context) = entries.filter { it.shown(context) }.toSet()
    }
}
