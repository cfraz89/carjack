package io.github.cfraz89.slipstream.launcher

import android.app.WallpaperManager
import android.content.BroadcastReceiver
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.res.Configuration
import android.graphics.drawable.Drawable
import android.os.BatteryManager
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.text.format.DateFormat
import android.window.OnBackInvokedCallback
import android.window.OnBackInvokedDispatcher
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.systemBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.wrapContentHeight
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicText
import androidx.compose.foundation.text.TextAutoSize
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.drawscope.drawIntoCanvas
import androidx.compose.ui.graphics.nativeCanvas
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.layout.positionInWindow
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.dimensionResource
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import kotlinx.coroutines.delay
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/** The home screen: status, what's playing, a grid of shortcut slots, and the taskbar. */
class HomeActivity : ComponentActivity() {

    private companion object {
        const val STATE_PENDING_SLOT = "pending_slot"
        const val WEATHER_REFRESH_MS = 15 * 60 * 1000L
    }

    /** The phone's charge, shown under the weather: the phone is in a pocket, out of sight. */
    private data class Charge(val percent: Int, val charging: Boolean)

    /** Slot awaiting a result from the picker; -1 when the picker is not open. */
    private var pendingSlot = -1

    /** Home is the root of the stack, so back has nowhere to go. */
    private val swallowBack = OnBackInvokedCallback { }

    private lateinit var nowPlaying: NowPlaying
    private val handler = Handler(Looper.getMainLooper())

    private var shownWallpaper by mutableStateOf<Drawable?>(null)
    private var wallpaperSize = IntSize.Zero
    private var wallpaperAt = Offset.Zero
    private var loadedWallpaper: String? = null
    private var wallpaperGeneration = 0
    private var weather by mutableStateOf<Weather.Now?>(null)
    private var charge by mutableStateOf<Charge?>(null)
    private var columns by mutableIntStateOf(Prefs.DEFAULT_COLUMNS)
    private var slots by mutableStateOf<List<AppEntry?>>(emptyList())
    private var recents by mutableStateOf<List<AppEntry>>(emptyList())
    /** The slot whose long-press menu is open, with its app's label. */
    private var slotMenu by mutableStateOf<Pair<Int, String>?>(null)

    private val pick = registerForActivityResult(ActivityResultContracts.StartActivityForResult()) {
        val slot = pendingSlot
        pendingSlot = -1
        val flat = it.data?.getStringExtra(AllAppsActivity.EXTRA_COMPONENT)
        if (it.resultCode == RESULT_OK && slot >= 0 && flat != null) {
            ComponentName.unflattenFromString(flat)?.let { c -> ShortcutStore.set(this, slot, c) }
            refreshGrid()
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.setDecorFitsSystemWindows(false)
        pendingSlot = savedInstanceState?.getInt(STATE_PENDING_SLOT, -1) ?: -1
        nowPlaying = NowPlaying(this)

        // On the phone, back leaves it like any app.
        if (!SystemBars.onPhone(this)) {
            onBackInvokedDispatcher.registerOnBackInvokedCallback(
                OnBackInvokedDispatcher.PRIORITY_DEFAULT,
                swallowBack,
            )
        }
        WallpaperManager.getInstance(this)
            .addOnColorsChangedListener(wallpaperChanged, Handler(mainLooper))
        Wallpapers.watch(this)

        setContent { LauncherTheme { Home() } }
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
        refreshGrid()
        registerReceiver(battery, IntentFilter(Intent.ACTION_BATTERY_CHANGED))
        refreshWeather.run()
        nowPlaying.start()
    }

    override fun onPause() {
        nowPlaying.stop()
        unregisterReceiver(battery)
        handler.removeCallbacks(refreshWeather)
        super.onPause()
    }

    override fun onSaveInstanceState(outState: Bundle) {
        super.onSaveInstanceState(outState)
        outState.putInt(STATE_PENDING_SLOT, pendingSlot)
    }

    /** Declared in the manifest, so rebuild rather than being recreated. */
    override fun onConfigurationChanged(newConfig: Configuration) {
        super.onConfigurationChanged(newConfig)
        refreshGrid()
    }

    private val battery = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            val level = intent.getIntExtra(BatteryManager.EXTRA_LEVEL, -1)
            val scale = intent.getIntExtra(BatteryManager.EXTRA_SCALE, 100)
            val charging = intent.getIntExtra(BatteryManager.EXTRA_PLUGGED, 0) != 0
            charge = if (level < 0) null else Charge(level * 100 / scale, charging)
        }
    }

    /** Off the main thread, and again every quarter hour while home is showing. */
    private val refreshWeather = object : Runnable {
        override fun run() {
            Thread({
                val now = Weather.now(this@HomeActivity)
                runOnUiThread { if (!isDestroyed) weather = now }
            }, "slipstream-weather").start()
            handler.postDelayed(this, WEATHER_REFRESH_MS)
        }
    }

    /** The slots and the taskbar's recents; a slot whose app has gone becomes addable again. */
    private fun refreshGrid() {
        columns = Prefs.columns(this)
        slots = (0 until Prefs.slotCount(this)).map { index ->
            val stored = ShortcutStore.get(this, index)
            val entry = stored?.let { AppRepository.entryFor(this, it) }
            if (stored != null && entry == null) ShortcutStore.clear(this, index)
            entry
        }
        recents = RecentsStore.visible(this, Prefs.recentCount(this))
    }

    /**
     * The chosen wallpaper ([Wallpapers]), framed for this screen, so it needs the laid-out size;
     * read again only when the choice, the phone's wallpaper or the size changes.
     */
    private fun loadWallpaper() {
        val (width, height) = wallpaperSize
        if (width == 0 || height == 0) return
        val key = "${Wallpapers.stamp(this)} ${width}x$height $wallpaperGeneration"
        if (key == loadedWallpaper) return
        loadedWallpaper = key
        // Kept across launches: fetching it can take a second, through Shizuku.
        Wallpapers.cached(key)?.let {
            showWallpaper(it)
            return
        }
        Thread({
            val drawable = Wallpapers.load(this, width, height)
            Wallpapers.keep(key, drawable)
            runOnUiThread {
                if (!isDestroyed && key == loadedWallpaper) showWallpaper(drawable)
            }
        }, "slipstream-wallpaper").start()
    }

    /** Shown, and its blurred copy cut for the panels. */
    private fun showWallpaper(drawable: Drawable) {
        shownWallpaper = drawable
        Blur.setWallpaper(
            drawable, wallpaperSize.width, wallpaperSize.height, wallpaperAt,
            resources.getDimension(R.dimen.blur_radius),
        )
    }

    private val wallpaperChanged = WallpaperManager.OnColorsChangedListener { _, _ ->
        runOnUiThread {
            wallpaperGeneration++
            if (!isDestroyed) loadWallpaper()
        }
    }

    private fun pickFor(index: Int) {
        pendingSlot = index
        pick.launch(
            Intent(this, AllAppsActivity::class.java).putExtra(AllAppsActivity.EXTRA_PICK_MODE, true),
        )
    }

    @Composable
    private fun Home() {
        Box(Modifier.fillMaxSize().background(Palette.background)) {
            DrawableImage(
                shownWallpaper,
                Modifier
                    .fillMaxSize()
                    .onGloballyPositioned {
                        if (it.size != wallpaperSize) {
                            wallpaperSize = it.size
                            wallpaperAt = it.positionInWindow()
                            shownWallpaper?.let(::showWallpaper)
                            loadWallpaper()
                        }
                    },
                crop = true,
            )
            // One tint over the whole wallpaper, deepest at the bottom.
            Box(
                Modifier.fillMaxSize().background(
                    Brush.verticalGradient(
                        0f to Color(0x66000000),
                        0.5f to Color(0x73000000),
                        1f to Color(0xBF000000),
                    ),
                ),
            )
            Column(Modifier.fillMaxSize().systemBarsPadding()) {
                Status()
                NowPlayingPanel(nowPlaying)
                Grid(Modifier.weight(1f))
                Taskbar()
            }
            slotMenu?.let { (index, label) -> SlotMenu(index, label) }
        }
    }

    /** Time and date; the weather where the phone is (the unit shows none) and its battery. */
    @Composable
    private fun Status() {
        val now = rememberMinute()
        val context = LocalContext.current
        val locale = Locale.getDefault()
        val time = SimpleDateFormat(
            DateFormat.getBestDateTimePattern(
                locale, if (DateFormat.is24HourFormat(context)) "Hm" else "hm",
            ),
            locale,
        ).format(Date(now))
        val date = SimpleDateFormat(DateFormat.getBestDateTimePattern(locale, "EEEEdMMMM"), locale)
            .format(Date(now))
        Row(
            Modifier
                .fillMaxWidth()
                .padding(
                    start = dimensionResource(R.dimen.status_padding),
                    end = dimensionResource(R.dimen.status_padding),
                    top = dimensionResource(R.dimen.status_padding_vertical),
                ),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Column(Modifier.weight(1f)) {
                // One line, scaled to fit rather than wrapped.
                BasicText(
                    time,
                    style = hudText(sp(R.dimen.status_clock_max), FontWeight.SemiBold),
                    maxLines = 1,
                    autoSize = TextAutoSize.StepBased(
                        sp(R.dimen.status_clock_min),
                        sp(R.dimen.status_clock_max),
                    ),
                    modifier = Modifier
                        .fillMaxWidth()
                        .height(dimensionResource(R.dimen.status_clock_height))
                        .wrapContentHeight(Alignment.CenterVertically),
                )
                Text(
                    date.uppercase(locale),
                    style = hudCaption(),
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            Column(horizontalAlignment = Alignment.End) {
                weather?.let { now ->
                    Row(
                        Modifier.height(dimensionResource(R.dimen.status_clock_height)),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Icon(
                            painterResource(now.icon),
                            contentDescription = null,
                            tint = Palette.accent,
                            modifier = Modifier.size(dimensionResource(R.dimen.status_icon_size)),
                        )
                        Spacer(Modifier.width(dimensionResource(R.dimen.status_icon_gap)))
                        Text(
                            stringResource(R.string.status_temperature, now.temperature),
                            style = hudText(sp(R.dimen.status_temperature_size), FontWeight.SemiBold),
                        )
                    }
                }
                Row(verticalAlignment = Alignment.CenterVertically) {
                    val sky = weather?.sky.orEmpty()
                    if (sky.isNotEmpty()) {
                        Text(sky.uppercase(locale), style = hudCaption(), maxLines = 1)
                    }
                    charge?.let { battery ->
                        Spacer(Modifier.width(dimensionResource(R.dimen.status_battery_gap)))
                        BatteryIcon(battery)
                        Spacer(Modifier.width(dimensionResource(R.dimen.status_battery_text_gap)))
                        Text(
                            stringResource(R.string.status_battery, battery.percent),
                            style = hudCaption(),
                            maxLines = 1,
                        )
                    }
                }
            }
        }
    }

    /** ic_battery's fill is a clip drawable, cut to the charge by the drawable's level. */
    @Composable
    private fun BatteryIcon(battery: Charge) {
        val context = LocalContext.current
        val drawable = remember { context.getDrawable(R.drawable.ic_battery)!!.mutate() }
        val tint = (if (battery.charging) Palette.accent else Palette.textSecondary).toArgb()
        // The fill spans x 4.25-18.25 of the icon's 24; the clip runs across all 24.
        val level = ((4.25f + battery.percent * 0.14f) / 24f * 10000).toInt()
        Canvas(Modifier.size(dimensionResource(R.dimen.status_battery_size))) {
            drawable.level = level
            drawable.setTint(tint)
            drawable.setBounds(0, 0, size.width.toInt(), size.height.toInt())
            drawIntoCanvas { drawable.draw(it.nativeCanvas) }
        }
    }

    @Composable
    private fun Grid(modifier: Modifier) {
        val margin = dimensionResource(R.dimen.shortcut_margin)
        Column(modifier.fillMaxWidth().padding(dimensionResource(R.dimen.grid_padding))) {
            slots.chunked(columns.coerceAtLeast(1)).forEachIndexed { row, line ->
                Row(Modifier.weight(1f).fillMaxWidth()) {
                    line.forEachIndexed { column, entry ->
                        Slot(
                            row * columns + column,
                            entry,
                            Modifier.weight(1f).fillMaxHeight().padding(margin),
                        )
                    }
                }
            }
        }
    }

    @Composable
    private fun Slot(index: Int, entry: AppEntry?, modifier: Modifier) {
        if (entry == null) {
            // Just the ringed plus, in the middle of the cell: it reads as "add" without a word.
            Box(
                modifier.tappable(CardShape) { pickFor(index) }.alpha(0.75f),
                contentAlignment = Alignment.Center,
            ) {
                Image(
                    painterResource(R.drawable.ic_add_slot),
                    contentDescription = stringResource(R.string.empty_slot),
                    modifier = Modifier.size(dimensionResource(R.dimen.add_slot_size)),
                )
            }
            return
        }
        Column(
            modifier
                .tappable(CardShape, onLongClick = { slotMenu = index to entry.label }) {
                    AppRepository.launch(this, entry.component)
                }
                .padding(dimensionResource(R.dimen.shortcut_padding)),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.Center,
        ) {
            // Weighted, so the label keeps its line and the icon gives up the space on a short cell.
            DrawableImage(
                entry.icon,
                Modifier
                    .weight(1f, fill = false)
                    .heightIn(
                        min = dimensionResource(R.dimen.shortcut_icon_min),
                        max = dimensionResource(R.dimen.shortcut_icon_size),
                    )
                    .aspectRatio(1f),
            )
            Spacer(Modifier.height(dimensionResource(R.dimen.shortcut_label_top)))
            Text(
                entry.label,
                style = hudText(sp(R.dimen.shortcut_label_size)).copy(letterSpacing = 0.04.em),
                textAlign = TextAlign.Center,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.fillMaxWidth(),
            )
        }
    }

    @Composable
    private fun SlotMenu(index: Int, label: String) {
        AlertDialog(
            onDismissRequest = { slotMenu = null },
            title = { Text(label) },
            confirmButton = {
                TextButton(onClick = {
                    slotMenu = null
                    pickFor(index)
                }) { Text(stringResource(R.string.change_app)) }
            },
            dismissButton = {
                TextButton(onClick = {
                    slotMenu = null
                    ShortcutStore.clear(this, index)
                    refreshGrid()
                }) { Text(stringResource(R.string.remove_shortcut)) }
            },
        )
    }

    /**
     * All Apps, a divider, then the recents. Settings sits at the far end, mirroring the way the
     * tablet taskbar keeps its buttons away from the icon group.
     */
    @Composable
    private fun Taskbar() {
        val margin = dimensionResource(R.dimen.taskbar_margin)
        Row(
            Modifier
                .fillMaxWidth()
                .padding(start = margin, end = margin, bottom = margin)
                .height(dimensionResource(R.dimen.taskbar_height)),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Row(
                Modifier
                    .padding(start = dimensionResource(R.dimen.taskbar_side_padding))
                    .height(dimensionResource(R.dimen.taskbar_button_size))
                    .tappable(CardShape) {
                        startActivity(Intent(this@HomeActivity, AllAppsActivity::class.java))
                    }
                    .padding(horizontal = dimensionResource(R.dimen.taskbar_button_padding)),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Image(
                    painterResource(R.drawable.ic_apps),
                    contentDescription = null,
                    modifier = Modifier.size(dimensionResource(R.dimen.taskbar_apps_icon_size)),
                )
                Spacer(Modifier.width(dimensionResource(R.dimen.taskbar_button_label_gap)))
                Text(
                    stringResource(R.string.apps).uppercase(),
                    style = hudText(sp(R.dimen.taskbar_button_label_size), FontWeight.SemiBold)
                        .copy(letterSpacing = 0.12.em),
                )
            }
            if (recents.isNotEmpty()) {
                Box(
                    Modifier
                        .padding(horizontal = dimensionResource(R.dimen.divider_margin))
                        .size(
                            dimensionResource(R.dimen.divider_width),
                            dimensionResource(R.dimen.divider_height),
                        )
                        .background(Palette.divider, RoundedCornerShape(1.dp)),
                )
            }
            Row(Modifier.weight(1f), verticalAlignment = Alignment.CenterVertically) {
                recents.forEach { entry ->
                    DrawableImage(
                        entry.icon,
                        Modifier
                            .padding(end = dimensionResource(R.dimen.taskbar_item_spacing))
                            .size(dimensionResource(R.dimen.taskbar_icon_size))
                            .tappable(CardShape) { AppRepository.launch(this@HomeActivity, entry.component) }
                            .padding(dimensionResource(R.dimen.taskbar_icon_padding)),
                    )
                }
            }
            // Icon only: the gear reads on its own, and a label would eat taskbar width.
            Image(
                painterResource(R.drawable.settings_24),
                contentDescription = stringResource(R.string.settings),
                modifier = Modifier
                    .padding(end = dimensionResource(R.dimen.taskbar_side_padding))
                    .size(dimensionResource(R.dimen.taskbar_button_size))
                    .tappable(CircleShape) {
                        startActivity(Intent(this@HomeActivity, SettingsActivity::class.java))
                    }
                    .padding(dimensionResource(R.dimen.icon_button_padding)),
            )
        }
    }
}

/** The time, ticking over on the minute. */
@Composable
private fun rememberMinute(): Long {
    var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(Unit) {
        while (true) {
            delay(60_000 - System.currentTimeMillis() % 60_000)
            now = System.currentTimeMillis()
        }
    }
    return now
}
