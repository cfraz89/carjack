package com.carjack.launcher

import android.app.Activity
import android.content.Intent
import android.os.Build
import android.os.Bundle
import android.view.LayoutInflater
import android.view.View
import android.view.ViewGroup
import android.view.WindowInsets
import android.widget.BaseAdapter
import android.widget.GridView
import android.widget.ImageView
import android.widget.TextView

/**
 * Every launchable app, as a bottom sheet over home. Doubles as the slot picker: in pick mode a
 * tap returns the chosen component instead of launching it.
 */
class AllAppsActivity : Activity() {

    companion object {
        const val EXTRA_PICK_MODE = "com.carjack.launcher.PICK_MODE"
        const val EXTRA_COMPONENT = "com.carjack.launcher.COMPONENT"
    }

    private var pickMode = false
    private var apps: List<AppEntry> = emptyList()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_all_apps)

        // Up from the bottom and back down, with home held still underneath.
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            overrideActivityTransition(OVERRIDE_TRANSITION_OPEN, R.anim.sheet_enter, R.anim.hold)
            overrideActivityTransition(OVERRIDE_TRANSITION_CLOSE, R.anim.hold, R.anim.sheet_exit)
        }

        pickMode = intent.getBooleanExtra(EXTRA_PICK_MODE, false)
        findViewById<TextView>(R.id.all_apps_title)
            .setText(if (pickMode) R.string.pick_an_app else R.string.all_apps)

        findViewById<View>(R.id.all_apps_header).setOnClickListener { finish() }

        apps = AppRepository.launchableApps(this)

        val grid = findViewById<GridView>(R.id.all_apps_grid)
        fitSheetToBars(findViewById(R.id.all_apps_sheet), grid)
        grid.adapter = AppAdapter()
        grid.setOnItemClickListener { _, _, position, _ -> choose(apps[position]) }
    }

    /**
     * The window is edge to edge, so the sheet runs to the bottom edge behind the navigation bar,
     * and only its top and the grid's scroll padding step clear of the bars.
     */
    private fun fitSheetToBars(sheet: View, grid: GridView) {
        val gap = resources.getDimensionPixelSize(R.dimen.sheet_top_gap)
        val gridBottom = grid.paddingBottom
        sheet.setOnApplyWindowInsetsListener { view, insets ->
            val bars = insets.getInsets(WindowInsets.Type.systemBars())
            val params = view.layoutParams as ViewGroup.MarginLayoutParams
            params.setMargins(bars.left, bars.top + gap, bars.right, 0)
            view.layoutParams = params
            grid.setPadding(
                grid.paddingLeft,
                grid.paddingTop,
                grid.paddingRight,
                gridBottom + bars.bottom,
            )
            insets
        }
    }

    override fun onResume() {
        super.onResume()
        SystemBars.hide(this)
    }

    private fun choose(entry: AppEntry) {
        if (pickMode) {
            setResult(
                RESULT_OK,
                Intent().putExtra(EXTRA_COMPONENT, entry.component.flattenToString()),
            )
            finish()
        } else if (AppRepository.launch(this, entry.component)) {
            // Otherwise the drawer is still open over home when the app is left.
            finish()
        }
    }

    private inner class AppAdapter : BaseAdapter() {
        override fun getCount(): Int = apps.size

        override fun getItem(position: Int): Any = apps[position]

        override fun getItemId(position: Int): Long = position.toLong()

        override fun getView(position: Int, convertView: View?, parent: ViewGroup): View {
            val view = convertView
                ?: LayoutInflater.from(parent.context).inflate(R.layout.item_app, parent, false)
            val entry = apps[position]
            view.findViewById<ImageView>(R.id.app_icon).setImageDrawable(entry.icon)
            view.findViewById<TextView>(R.id.app_label).text = entry.label
            return view
        }
    }
}
