package com.carjack.launcher

import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.drawable.Drawable
import android.util.Log

/** One launchable activity, resolved for display. */
data class AppEntry(
    val component: ComponentName,
    val label: String,
    val icon: Drawable,
)

object AppRepository {

    private const val TAG = "CarjackLauncher"

    /** Every launchable activity except our own, sorted by label. */
    fun launchableApps(context: Context): List<AppEntry> {
        val pm = context.packageManager
        val query = Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER)
        @Suppress("DEPRECATION")
        return pm.queryIntentActivities(query, 0)
            .asSequence()
            .filter { it.activityInfo.packageName != context.packageName }
            .map { resolved ->
                AppEntry(
                    component = ComponentName(
                        resolved.activityInfo.packageName,
                        resolved.activityInfo.name,
                    ),
                    label = resolved.loadLabel(pm).toString(),
                    icon = resolved.loadIcon(pm),
                )
            }
            .sortedBy { it.label.lowercase() }
            .toList()
    }

    /** Null when the component no longer resolves - uninstalled, or disabled. */
    fun entryFor(context: Context, component: ComponentName): AppEntry? {
        val pm = context.packageManager
        return try {
            @Suppress("DEPRECATION")
            val info = pm.getActivityInfo(component, 0)
            AppEntry(component, info.loadLabel(pm).toString(), info.loadIcon(pm))
        } catch (e: PackageManager.NameNotFoundException) {
            null
        }
    }

    fun launch(context: Context, component: ComponentName): Boolean {
        val intent = Intent(Intent.ACTION_MAIN)
            .addCategory(Intent.CATEGORY_LAUNCHER)
            .setComponent(component)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_RESET_TASK_IF_NEEDED)
        return try {
            context.startActivity(intent)
            RecentsStore.record(context, component)
            true
        } catch (e: Exception) {
            Log.w(TAG, "could not launch $component", e)
            false
        }
    }
}
