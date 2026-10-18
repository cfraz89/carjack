package com.carjack.launcher

import android.app.Activity
import android.app.AlertDialog
import android.app.WallpaperManager
import android.content.ComponentName
import android.content.Intent
import android.content.res.Configuration
import android.os.Bundle
import android.view.LayoutInflater
import android.view.View
import android.widget.GridLayout
import android.widget.ImageView
import android.widget.TextView
import android.window.OnBackInvokedCallback
import android.window.OnBackInvokedDispatcher

/** The home screen: a grid of shortcut slots, with the taskbar underneath. */
class HomeActivity : Activity() {

    private companion object {
        const val REQUEST_PICK = 1
        const val STATE_PENDING_SLOT = "pending_slot"
    }

    private lateinit var grid: GridLayout
    private lateinit var taskbar: TaskbarView
    private lateinit var wallpaper: ImageView

    /** Slot awaiting a result from the picker; -1 when the picker is not open. */
    private var pendingSlot = -1

    /** Home is the root of the stack, so back has nowhere to go. */
    private val swallowBack = OnBackInvokedCallback { }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_home)

        grid = findViewById(R.id.shortcut_grid)
        taskbar = findViewById(R.id.taskbar)
        wallpaper = findViewById(R.id.wallpaper)
        taskbar.onAllApps = { startActivity(Intent(this, AllAppsActivity::class.java)) }
        taskbar.onSettings = { startActivity(Intent(this, SettingsActivity::class.java)) }

        pendingSlot = savedInstanceState?.getInt(STATE_PENDING_SLOT, -1) ?: -1

        onBackInvokedDispatcher.registerOnBackInvokedCallback(
            OnBackInvokedDispatcher.PRIORITY_DEFAULT,
            swallowBack,
        )
    }

    override fun onDestroy() {
        onBackInvokedDispatcher.unregisterOnBackInvokedCallback(swallowBack)
        super.onDestroy()
    }

    override fun onResume() {
        super.onResume()
        SystemBars.hide(this)
        loadWallpaper()
        buildGrid()
        taskbar.refresh()
    }

    /** The system's built-in wallpaper: an ordinary app may not read the phone's own. */
    private fun loadWallpaper() {
        wallpaper.setImageDrawable(WallpaperManager.getInstance(this).builtInDrawable)
    }

    override fun onSaveInstanceState(outState: Bundle) {
        super.onSaveInstanceState(outState)
        outState.putInt(STATE_PENDING_SLOT, pendingSlot)
    }

    /** Declared in the manifest, so rebuild rather than being recreated. */
    override fun onConfigurationChanged(newConfig: Configuration) {
        super.onConfigurationChanged(newConfig)
        buildGrid()
        taskbar.refresh()
    }

    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode != REQUEST_PICK || resultCode != RESULT_OK) return
        val slot = pendingSlot
        pendingSlot = -1
        if (slot < 0) return
        val flat = data?.getStringExtra(AllAppsActivity.EXTRA_COMPONENT) ?: return
        ComponentName.unflattenFromString(flat)?.let { ShortcutStore.set(this, slot, it) }
    }

    private fun buildGrid() {
        val columns = Prefs.columns(this)
        val rows = Prefs.rows(this)
        val margin = resources.getDimensionPixelSize(R.dimen.shortcut_margin)
        val inflater = LayoutInflater.from(this)

        grid.removeAllViews()
        grid.columnCount = columns
        grid.rowCount = rows

        for (index in 0 until columns * rows) {
            val cell = inflater.inflate(R.layout.item_shortcut, grid, false)
            cell.layoutParams = GridLayout.LayoutParams(
                GridLayout.spec(index / columns, 1, 1f),
                GridLayout.spec(index % columns, 1, 1f),
            ).apply {
                width = 0
                height = 0
                setMargins(margin, margin, margin, margin)
            }
            bindCell(cell, index)
            grid.addView(cell)
        }
    }

    private val blurRadius: Float
        get() = resources.getDimension(R.dimen.blur_radius)

    private fun bindCell(cell: View, index: Int) {
        val icon = cell.findViewById<ImageView>(R.id.shortcut_icon)
        val label = cell.findViewById<TextView>(R.id.shortcut_label)
        val stored = ShortcutStore.get(this, index)
        val entry = stored?.let { AppRepository.entryFor(this, it) }

        if (entry == null) {
            // Drop a shortcut whose app has gone, so the slot returns to being addable.
            if (stored != null) ShortcutStore.clear(this, index)
            icon.setImageResource(R.drawable.ic_add_slot)
            icon.contentDescription = getString(R.string.empty_slot)
            label.setText(R.string.empty_slot)
            // Only an empty slot gets a plate; a filled one sits straight on the wallpaper.
            cell.setBackgroundResource(R.drawable.slot_background)
            cell.clipToOutline = true
            Blur.apply(cell, blurRadius)
            cell.alpha = 0.7f
            cell.isLongClickable = false
            cell.setOnLongClickListener(null)
            cell.setOnClickListener { pickFor(index) }
        } else {
            icon.setImageDrawable(entry.icon)
            icon.contentDescription = entry.label
            label.text = entry.label
            cell.setBackgroundResource(R.drawable.ripple_transparent)
            cell.clipToOutline = false
            Blur.clear(cell)
            cell.alpha = 1f
            cell.setOnClickListener { AppRepository.launch(this, entry.component) }
            cell.setOnLongClickListener {
                showSlotMenu(index, entry.label)
                true
            }
        }
    }

    private fun pickFor(index: Int) {
        pendingSlot = index
        startActivityForResult(
            Intent(this, AllAppsActivity::class.java)
                .putExtra(AllAppsActivity.EXTRA_PICK_MODE, true),
            REQUEST_PICK,
        )
    }

    private fun showSlotMenu(index: Int, label: String) {
        val options = arrayOf(
            getString(R.string.change_app),
            getString(R.string.remove_shortcut),
        )
        AlertDialog.Builder(this, R.style.CarjackDialog)
            .setTitle(label)
            .setItems(options) { _, which ->
                when (which) {
                    0 -> pickFor(index)
                    1 -> {
                        ShortcutStore.clear(this, index)
                        buildGrid()
                        taskbar.refresh()
                    }
                }
            }
            .show()
    }
}
