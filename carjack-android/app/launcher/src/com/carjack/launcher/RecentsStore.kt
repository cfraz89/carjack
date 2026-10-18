package com.carjack.launcher

import android.content.ComponentName
import android.content.Context

/**
 * Recents we track ourselves, rather than reading the system task list.
 *
 * ActivityManager.getRecentTasks needs a signature permission we would only hold as a
 * platform-signed system app, and it reports whatever the system last ran - including
 * things the rider never chose. Recording our own launches keeps the bar to apps that
 * were actually opened from the launcher, which is both permission-free and steadier:
 * positions only change when the rider opens something.
 */
object RecentsStore {

    private const val KEY = "recent_components"
    private const val SEPARATOR = "\n"
    private const val CAP = 16

    fun record(context: Context, component: ComponentName) {
        val flat = component.flattenToString()
        val current = stored(context).toMutableList()
        current.remove(flat)
        current.add(0, flat)
        while (current.size > CAP) {
            current.removeAt(current.size - 1)
        }
        Prefs.get(context).edit().putString(KEY, current.joinToString(SEPARATOR)).apply()
    }

    /**
     * What the taskbar should show: most recent first, dropping anything already pinned
     * to the home grid so the bar never duplicates a shortcut, and anything that no
     * longer resolves.
     */
    fun visible(context: Context, limit: Int): List<AppEntry> {
        if (limit <= 0) return emptyList()
        val pinned = ShortcutStore.pinned(context)
        return stored(context)
            .asSequence()
            .mapNotNull { ComponentName.unflattenFromString(it) }
            .filter { it !in pinned }
            .mapNotNull { AppRepository.entryFor(context, it) }
            .take(limit)
            .toList()
    }

    private fun stored(context: Context): List<String> {
        val raw = Prefs.get(context).getString(KEY, "").orEmpty()
        return if (raw.isEmpty()) emptyList() else raw.split(SEPARATOR)
    }
}
