package com.carjack.launcher

import android.content.ComponentName
import android.content.Context

/**
 * Slot index -> component. Slots are addressed by grid position, so changing the
 * grid dimensions keeps whatever still fits and leaves the rest stored but unshown.
 */
object ShortcutStore {

    private fun key(index: Int) = "shortcut_$index"

    fun get(context: Context, index: Int): ComponentName? =
        Prefs.get(context).getString(key(index), null)
            ?.let { ComponentName.unflattenFromString(it) }

    fun set(context: Context, index: Int, component: ComponentName) {
        Prefs.get(context).edit().putString(key(index), component.flattenToString()).apply()
    }

    fun clear(context: Context, index: Int) {
        Prefs.get(context).edit().remove(key(index)).apply()
    }

    /** Components currently pinned to a visible slot. */
    fun pinned(context: Context): Set<ComponentName> {
        val result = mutableSetOf<ComponentName>()
        for (index in 0 until Prefs.slotCount(context)) {
            get(context, index)?.let { result.add(it) }
        }
        return result
    }
}
