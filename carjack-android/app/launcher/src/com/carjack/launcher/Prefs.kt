package com.carjack.launcher

import android.content.Context
import android.content.SharedPreferences

/** Every user-adjustable number, with its bounds. */
object Prefs {
    private const val FILE = "carjack_launcher"

    private const val KEY_COLUMNS = "grid_columns"
    private const val KEY_ROWS = "grid_rows"
    private const val KEY_RECENTS = "recent_count"

    const val DEFAULT_COLUMNS = 3
    const val DEFAULT_ROWS = 2
    const val DEFAULT_RECENTS = 4

    const val MIN_COLUMNS = 1
    const val MAX_COLUMNS = 8
    const val MIN_ROWS = 1
    const val MAX_ROWS = 6
    const val MIN_RECENTS = 0
    const val MAX_RECENTS = 8

    fun get(context: Context): SharedPreferences =
        context.applicationContext.getSharedPreferences(FILE, Context.MODE_PRIVATE)

    fun columns(context: Context): Int = get(context).getInt(KEY_COLUMNS, DEFAULT_COLUMNS)

    fun rows(context: Context): Int = get(context).getInt(KEY_ROWS, DEFAULT_ROWS)

    fun recentCount(context: Context): Int = get(context).getInt(KEY_RECENTS, DEFAULT_RECENTS)

    fun slotCount(context: Context): Int = columns(context) * rows(context)

    fun setColumns(context: Context, value: Int) =
        put(context, KEY_COLUMNS, value.coerceIn(MIN_COLUMNS, MAX_COLUMNS))

    fun setRows(context: Context, value: Int) =
        put(context, KEY_ROWS, value.coerceIn(MIN_ROWS, MAX_ROWS))

    fun setRecentCount(context: Context, value: Int) =
        put(context, KEY_RECENTS, value.coerceIn(MIN_RECENTS, MAX_RECENTS))

    private fun put(context: Context, key: String, value: Int) {
        get(context).edit().putInt(key, value).apply()
    }
}
