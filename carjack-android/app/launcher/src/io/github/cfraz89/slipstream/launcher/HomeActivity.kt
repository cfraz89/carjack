package io.github.cfraz89.slipstream.launcher

import android.app.Activity
import android.app.AlertDialog
import android.app.WallpaperManager
import android.content.BroadcastReceiver
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.res.Configuration
import android.os.BatteryManager
import android.os.Bundle
import android.os.Handler
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
        const val WEATHER_REFRESH_MS = 15 * 60 * 1000L
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
        status = findViewById(R.id.status)
        temperature = findViewById(R.id.status_temperature)
        weather = findViewById(R.id.status_weather)
        weatherIcon = findViewById(R.id.status_weather_icon)
        statusDetail = findViewById(R.id.status_detail)
        Blur.apply(status)
        val playingPanel = findViewById<View>(R.id.playing)
        Blur.apply(playingPanel)
        nowPlaying = NowPlaying(this, playingPanel)

        pendingSlot = savedInstanceState?.getInt(STATE_PENDING_SLOT, -1) ?: -1

        // On the phone, back leaves it like any app.
        if (!SystemBars.onPhone(this)) {
            onBackInvokedDispatcher.registerOnBackInvokedCallback(
                OnBackInvokedDispatcher.PRIORITY_DEFAULT,
                swallowBack,
            )
        }
        WallpaperManager.getInstance(this)
            .addOnColorsChangedListener(wallpaperChanged, Handler(mainLooper))
        // Both need the laid-out size, which onResume may come before.
        wallpaper.addOnLayoutChangeListener { _, left, top, right, bottom,
                                              oldLeft, oldTop, oldRight, oldBottom ->
            if (right - left != oldRight - oldLeft || bottom - top != oldBottom - oldTop) {
                loadWallpaper()
                blurWallpaper()
            }
        }
    }

    override fun onDestroy() {
        WallpaperManager.getInstance(this).removeOnColorsChangedListener(wallpaperChanged)
        onBackInvokedDispatcher.unregisterOnBackInvokedCallback(swallowBack)
        super.onDestroy()
    }

    override fun onResume() {
        super.onResume()
        SystemBars.hide(this)
        loadWallpaper()
        buildGrid()
        taskbar.refresh()
        registerReceiver(battery, IntentFilter(Intent.ACTION_BATTERY_CHANGED))
        refreshWeather.run()
        nowPlaying.start()
    }

    override fun onPause() {
        nowPlaying.stop()
        unregisterReceiver(battery)
        statusHandler.removeCallbacks(refreshWeather)
        super.onPause()
    }

    private lateinit var status: View
    private lateinit var nowPlaying: NowPlaying
    private lateinit var temperature: TextView
    private lateinit var weather: View
    private lateinit var weatherIcon: ImageView
    private lateinit var statusDetail: TextView
    private var sky = ""
    private var batteryLine = ""
    private val statusHandler = Handler(android.os.Looper.getMainLooper())

    /** The phone's charge, under the weather: it's in a pocket, out of sight. */
    private val battery = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            val level = intent.getIntExtra(BatteryManager.EXTRA_LEVEL, -1)
            val scale = intent.getIntExtra(BatteryManager.EXTRA_SCALE, 100)
            val charging = intent.getIntExtra(BatteryManager.EXTRA_PLUGGED, 0) != 0
            batteryLine = if (level < 0) "" else
                getString(R.string.status_battery, level * 100 / scale) +
                    if (charging) getString(R.string.status_charging) else ""
            showStatusDetail()
        }
    }

    /** Off the main thread, and again every quarter hour while home is showing. */
    private val refreshWeather = object : Runnable {
        override fun run() {
            Thread({
                val now = Weather.now(this@HomeActivity)
                runOnUiThread {
                    if (isDestroyed) return@runOnUiThread
                    weather.visibility = if (now == null) View.GONE else View.VISIBLE
                    if (now != null) {
                        temperature.text = getString(R.string.status_temperature, now.temperature)
                        weatherIcon.setImageResource(now.icon)
                    }
                    sky = now?.sky ?: ""
                    showStatusDetail()
                }
            }, "slipstream-weather").start()
            statusHandler.postDelayed(this, WEATHER_REFRESH_MS)
        }
    }

    private fun showStatusDetail() {
        statusDetail.text = listOf(sky, batteryLine).filter { it.isNotEmpty() }.joinToString("  ·  ")
    }

    /**
     * The chosen wallpaper ([Wallpapers]), framed for this screen, so it needs the laid-out size;
     * read again only when the choice, the phone's wallpaper or the size changes.
     */
    private fun loadWallpaper() {
        val width = wallpaper.width
        val height = wallpaper.height
        if (width == 0 || height == 0) return
        val key = "${Wallpapers.stamp(this)} ${width}x$height $wallpaperGeneration"
        if (key == loadedWallpaper) return
        loadedWallpaper = key
        Thread({
            val drawable = Wallpapers.load(this, width, height)
            runOnUiThread {
                if (isDestroyed || key != loadedWallpaper) return@runOnUiThread
                wallpaper.setImageDrawable(drawable)
                blurWallpaper()
            }
        }, "slipstream-wallpaper").start()
    }

    /** The panels' blurred copy, cut to the size the wallpaper is shown at. */
    private fun blurWallpaper() {
        Blur.setWallpaper(wallpaper.drawable, wallpaper, resources.getDimension(R.dimen.blur_radius))
    }

    private var loadedWallpaper: String? = null
    private var wallpaperGeneration = 0

    private val wallpaperChanged = WallpaperManager.OnColorsChangedListener { _, _ ->
        runOnUiThread {
            wallpaperGeneration++
            if (!isDestroyed) loadWallpaper()
        }
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

    private fun bindCell(cell: View, index: Int) {
        val icon = cell.findViewById<ImageView>(R.id.shortcut_icon)
        val label = cell.findViewById<TextView>(R.id.shortcut_label)
        val stored = ShortcutStore.get(this, index)
        val entry = stored?.let { AppRepository.entryFor(this, it) }

        // Every slot sits on a blurred plate, filled or not; an empty one is dimmer.
        cell.setBackgroundResource(R.drawable.slot_background)
        cell.clipToOutline = true
        Blur.apply(cell)
        if (entry == null) {
            // Drop a shortcut whose app has gone, so the slot returns to being addable.
            if (stored != null) ShortcutStore.clear(this, index)
            icon.setImageResource(R.drawable.ic_add_slot)
            icon.contentDescription = getString(R.string.empty_slot)
            label.setText(R.string.empty_slot)
            cell.alpha = 0.6f
            cell.isLongClickable = false
            cell.setOnLongClickListener(null)
            cell.setOnClickListener { pickFor(index) }
        } else {
            icon.setImageDrawable(entry.icon)
            icon.contentDescription = entry.label
            label.text = entry.label
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
        AlertDialog.Builder(this, R.style.SlipstreamDialog)
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
