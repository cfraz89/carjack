package io.github.cfraz89.slipstream

import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.drawable.Drawable
import io.github.cfraz89.slipstream.ui.R

/**
 * What the head unit opens on connect, as the "app" setting holds it: "" for the built-in launcher,
 * [Previews.HOME] for the home app, else a package. Names and icons for the UI.
 */
object Choices {
    const val LAUNCHER = ""

    fun current(context: Context) = Variant.setting(context, "app", Variant.ON_CONNECT.first())

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
        val home = homeApp(context)
        return if (home != null) appLabel(context, home.packageName) else "None set"
    }

    fun icon(context: Context, app: String): Drawable? {
        val pm = context.packageManager
        try {
            if (app == Previews.HOME) {
                homeApp(context)?.let { return pm.getApplicationIcon(it.packageName) }
            } else if (isApp(app)) {
                return pm.getApplicationIcon(app.substringBefore('/'))
            }
        } catch (ignored: PackageManager.NameNotFoundException) {
        }
        if (app == LAUNCHER) Variant.launcherIcon(context)?.let { return it }
        return context.getDrawable(if (app == Previews.HOME) R.drawable.ic_sl_home else R.drawable.ic_sl_apps)
    }

    /** The device's own home app, or null where none is set as the default. */
    fun homeApp(context: Context): ComponentName? {
        val info = context.packageManager.resolveActivity(
            Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_HOME),
            PackageManager.MATCH_DEFAULT_ONLY,
        )
        // With no default, the chooser resolves; it isn't a home app.
        val activity = info?.activityInfo ?: return null
        if (activity.packageName == "android") return null
        return ComponentName(activity.packageName, activity.name)
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
