package io.github.cfraz89.slipstream

import android.content.Context
import android.content.pm.PackageManager
import android.graphics.drawable.Drawable

/**
 * What the unit opens on connect, as the "app" setting holds it: "" for the built-in launcher,
 * [Previews.HOME] for the phone's home app, else a package. Names and icons for the UI.
 */
object Choices {
    const val LAUNCHER = ""

    fun current(context: Context) = Variant.setting(context, "app", LAUNCHER)

    fun choose(context: Context, app: String) {
        Variant.prefs(context).edit().putString("app", app).apply()
    }

    fun isApp(app: String) = app != LAUNCHER && app != Previews.HOME

    fun name(context: Context, app: String) = when (app) {
        LAUNCHER -> "Slipstream launcher"
        Previews.HOME -> "Home screen"
        else -> appLabel(context, app)
    }

    /** The detail under the name: whose home screen, or nothing. */
    fun detail(context: Context, app: String): String? {
        if (app != Previews.HOME) return null
        val home = Variant.homeApp(context)
        return if (home != null) appLabel(context, home.packageName) else "None set"
    }

    fun icon(context: Context, app: String): Drawable? {
        val pm = context.packageManager
        try {
            if (app == Previews.HOME) {
                Variant.homeApp(context)?.let { return pm.getApplicationIcon(it.packageName) }
            } else if (isApp(app)) {
                return pm.getApplicationIcon(app.substringBefore('/'))
            }
        } catch (ignored: PackageManager.NameNotFoundException) {
        }
        return context.getDrawable(
            when (app) {
                LAUNCHER -> R.drawable.ic_launcher
                Previews.HOME -> R.drawable.ic_sl_home
                else -> R.drawable.ic_sl_apps
            },
        )
    }

    private fun appLabel(context: Context, app: String): String {
        val pm = context.packageManager
        return try {
            pm.getApplicationLabel(pm.getApplicationInfo(app.substringBefore('/'), 0)).toString()
        } catch (e: PackageManager.NameNotFoundException) {
            app
        }
    }
}
