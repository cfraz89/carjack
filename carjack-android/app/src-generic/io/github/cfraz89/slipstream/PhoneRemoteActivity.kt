package io.github.cfraz89.slipstream

import android.app.PendingIntent
import android.app.PictureInPictureParams
import android.app.RemoteAction
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.content.res.Configuration
import android.graphics.drawable.Drawable
import android.graphics.drawable.Icon
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.util.Rational
import android.view.HapticFeedbackConstants
import android.view.KeyEvent
import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.waitForUpOrCancellation
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.slideOutVertically
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.asPaddingValues
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.onClick
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import io.github.cfraz89.slipstream.launcher.AppEntry
import io.github.cfraz89.slipstream.launcher.AppRepository
import io.github.cfraz89.slipstream.launcher.LauncherTheme
import io.github.cfraz89.slipstream.launcher.Prefs
import io.github.cfraz89.slipstream.launcher.RecentsStore
import io.github.cfraz89.slipstream.launcher.ShortcutStore
import io.github.cfraz89.slipstream.launcher.hudCaption
import io.github.cfraz89.slipstream.launcher.hudText
import io.github.cfraz89.slipstream.launcher.tappable
import java.lang.ref.WeakReference
import java.util.concurrent.Executors

/**
 * The phone's screen during a head unit session, in either mode: a remote for the head unit -
 * the controls chosen in settings ([RemoteControl]), sending what its switchgear would - and the
 * swipe that disconnects. It keeps the phone awake while it's showing; the head unit carries on
 * with the phone locked and its screen off all the same.
 */
class PhoneRemoteActivity : ComponentActivity() {
    companion object {
        /** A session is up, its display on the head unit. */
        var connected by mutableStateOf(false)
        /** What the head unit gets, for the title. */
        var mode by mutableStateOf("")
        /** Where things are up to, as the service says, and whether it's given up until a retry. */
        var status by mutableStateOf(SlipstreamService.WAITING)
        var retryable by mutableStateOf(false)

        private const val HEAD_UNIT_MS = 2_000L
        private const val ACTION_BACK = "io.github.cfraz89.slipstream.remote.BACK"
        private const val ACTION_HOME = "io.github.cfraz89.slipstream.remote.HOME"
        private const val KEYCODE_HOME = 3

        private const val EXTRA_PREVIEW = "io.github.cfraz89.slipstream.remote.PREVIEW"

        private var current = WeakReference<PhoneRemoteActivity>(null)
        /** Every app as last read, for the drawer: reading them, icons and all, takes a while. */
        @Volatile private var appsCache: List<AppEntry>? = null

        /** The phone remote as a session would show it, its controls going nowhere; the remote itself during one. */
        fun preview(context: Context) {
            context.startActivity(Intent(context, PhoneRemoteActivity::class.java).putExtra(EXTRA_PREVIEW, true))
        }

        fun close() {
            current.get()?.let { it.runOnUiThread { it.finish() } }
        }
    }

    private val main = Handler(Looper.getMainLooper())
    private val background = Executors.newSingleThreadExecutor()

    private var headUnitPackage by mutableStateOf<String?>(null)
    private var pip by mutableStateOf(false)
    private var controls by mutableStateOf(emptySet<RemoteControl>())
    /** The head unit launcher's grid: its slots in order, null where empty, and its width. */
    private var slots by mutableStateOf(emptyList<AppEntry?>())
    private var columns by mutableStateOf(Prefs.DEFAULT_COLUMNS)
    /** Its taskbar's recent apps. */
    private var recents by mutableStateOf(emptyList<AppEntry>())
    /** Every app, from the cache so the drawer opens at once. */
    private var allApps by mutableStateOf(appsCache.orEmpty())
    private var drawerOpen by mutableStateOf(false)
    /** Opened from settings without a session: no picture-in-picture, settings or status. */
    private var preview = false

    /** Slipstream's own colours (res/values-night), dark whatever the phone is set to. */
    private val night by lazy {
        createConfigurationContext(
            Configuration(resources.configuration).apply {
                uiMode = Configuration.UI_MODE_NIGHT_YES or (uiMode and Configuration.UI_MODE_NIGHT_MASK.inv())
            },
        )
    }
    private fun tone(id: Int) = Color(night.getColor(id))
    private val surface by lazy { tone(R.color.sl_surface) }
    private val onSurface by lazy { tone(R.color.sl_on_surface) }
    private val onSurfaceVariant by lazy { tone(R.color.sl_on_surface_variant) }
    private val neutral by lazy { tone(R.color.sl_surface_container_highest) }
    private val primaryContainer by lazy { tone(R.color.sl_primary_container) }
    private val onPrimaryContainer by lazy { tone(R.color.sl_on_primary_container) }
    private val tertiary by lazy { tone(R.color.sl_tertiary) }
    private val sheet by lazy { tone(R.color.sl_surface_container_high) }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        current = WeakReference(this)
        preview = intent.getBooleanExtra(EXTRA_PREVIEW, false) && !connected
        window.setDecorFitsSystemWindows(false)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        setPictureInPictureParams(pipParams())
        registerReceiver(
            pipAction,
            IntentFilter().apply {
                addAction(ACTION_BACK)
                addAction(ACTION_HOME)
            },
            RECEIVER_NOT_EXPORTED,
        )
        setContent { PhoneRemote() }
    }

    override fun onStart() {
        super.onStart()
        main.post(pollHeadUnit)
    }

    /** The controls afresh: they may have been changed in settings, opened from here. */
    override fun onResume() {
        super.onResume()
        controls = RemoteControl.shown(this)
        background.execute { refreshLauncher() }
        background.execute {
            val apps = AppRepository.launchableApps(this)
            appsCache = apps
            main.post { allApps = apps }
        }
    }

    /** The head unit launcher's grid and recents afresh. Off the main thread. */
    private fun refreshLauncher() {
        val width = Prefs.columns(this)
        val entries = (0 until Prefs.slotCount(this)).map { index ->
            ShortcutStore.get(this, index)?.let { AppRepository.entryFor(this, it) }
        }
        val recent = RecentsStore.visible(this, Prefs.recentCount(this))
        main.post {
            columns = width
            slots = entries
            recents = recent
        }
    }

    /** Apps go to the head unit in Slipstream mode, its launcher's shortcuts with them. */
    private val showsApps get() = RemoteControl.APPS in controls && SlipstreamMode.isOn(this)

    /** [entry] onto the head unit's display, brought over where it's running on the phone. */
    private fun open(entry: AppEntry) {
        if (AppRepository.bringOver?.invoke(entry.component) != true) return
        background.execute {
            RecentsStore.record(this, entry.component)
            refreshLauncher()
        }
    }

    override fun onStop() {
        main.removeCallbacks(pollHeadUnit)
        super.onStop()
    }

    override fun onDestroy() {
        if (current.get() === this) current = WeakReference(null)
        unregisterReceiver(pipAction)
        background.shutdown()
        super.onDestroy()
    }

    /**
     * Leaving shrinks it to a picture-in-picture window rather than closing it, with back and
     * home as the window's actions; expanding it brings the remote back.
     */
    private fun pipParams(): PictureInPictureParams = PictureInPictureParams.Builder()
        .setAspectRatio(Rational(1, 1))
        .setAutoEnterEnabled(!preview)
        .setActions(
            listOf(
                pipAction(ACTION_BACK, R.drawable.ic_sl_arrow_back, "Back"),
                pipAction(ACTION_HOME, R.drawable.ic_sl_home, "Home"),
            ),
        )
        .build()

    private fun pipAction(action: String, icon: Int, title: String) = RemoteAction(
        Icon.createWithResource(this, icon).setTint(android.graphics.Color.WHITE),
        title,
        title,
        PendingIntent.getBroadcast(this, 0, Intent(action).setPackage(packageName), PendingIntent.FLAG_IMMUTABLE),
    )

    private val pipAction = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            when (intent.action) {
                ACTION_BACK -> press(KeyEvent.KEYCODE_BACK)
                ACTION_HOME -> press(KEYCODE_HOME)
            }
        }
    }

    /**
     * A key as the head unit's switchgear would send it, so it goes where the switchgear's does;
     * back and home, the remote's own buttons, as they are.
     */
    private fun send(code: Int, down: Boolean) {
        background.execute {
            if (code == KeyEvent.KEYCODE_BACK || code == KEYCODE_HOME) {
                Variant.navigate(this, code, down)
            } else {
                Variant.switchgear(this, code, down)
            }
        }
    }

    private fun press(code: Int) {
        send(code, true)
        send(code, false)
    }

    override fun onPictureInPictureModeChanged(inPip: Boolean, newConfig: Configuration) {
        super.onPictureInPictureModeChanged(inPip, newConfig)
        pip = inPip
    }

    /** Out of the way as a picture-in-picture window, the phone's own screen showing beneath. */
    private fun shrink(): Boolean = try {
        enterPictureInPictureMode(pipParams())
    } catch (e: IllegalStateException) {
        false
    }

    /** The head unit disconnected, and this screen closed; see [SlipstreamService.ACTION_DISCONNECT]. */
    private fun end() {
        if (preview) {
            finish()
            return
        }
        startService(Intent(this, SlipstreamService::class.java).setAction(SlipstreamService.ACTION_DISCONNECT))
        finish()
    }

    /** What's on the head unit in Slipstream mode, for the picture-in-picture window. */
    private val pollHeadUnit = object : Runnable {
        override fun run() {
            background.execute {
                val top = ShizukuDisplay.topPackage()
                main.post { headUnitPackage = top }
            }
            main.postDelayed(this, HEAD_UNIT_MS)
        }
    }

    private fun appIcon(pkg: String?): Drawable? = when (pkg) {
        null, packageName -> getDrawable(R.mipmap.ic_sl_launcher)
        else -> try {
            packageManager.getApplicationIcon(pkg)
        } catch (e: PackageManager.NameNotFoundException) {
            null
        }
    }

    @Composable
    private fun PhoneRemote() = LauncherTheme {
        Box(Modifier.fillMaxSize().background(Color.Black)) {
            if (pip) {
                Box(Modifier.fillMaxSize().padding(12.dp), contentAlignment = Alignment.Center) {
                    val pkg = headUnitPackage
                    DrawableImage(remember(pkg) { appIcon(pkg) }, Modifier.fillMaxSize(0.7f))
                }
            } else {
                Controls()
                AnimatedVisibility(
                    drawerOpen,
                    enter = slideInVertically { it },
                    exit = slideOutVertically { it },
                ) {
                    AllApps()
                }
            }
        }
    }

    /**
     * The head unit launcher's grid as it's laid out there, empty slots and all, and its taskbar:
     * all apps, then the recent apps.
     */
    @Composable
    private fun Launcher(modifier: Modifier) {
        BoxWithConstraints(modifier) {
            val width = columns.coerceAtLeast(1)
            val rows = slots.chunked(width)
            val gridHeight = maxHeight - TASKBAR - 12.dp
            val cellHeight = gridHeight / rows.size.coerceAtLeast(1)
            val icon = minOf(maxWidth / width * 0.6f, cellHeight - 40.dp, 72.dp).coerceAtLeast(28.dp)
            Column(Modifier.fillMaxWidth(), horizontalAlignment = Alignment.CenterHorizontally) {
                Column(Modifier.fillMaxWidth()) {
                    for (row in rows) {
                        Row(Modifier.fillMaxWidth()) {
                            for (entry in row) {
                                Box(Modifier.weight(1f), contentAlignment = Alignment.Center) {
                                    if (entry == null) {
                                        Box(
                                            Modifier.size(icon)
                                                .border(1.5.dp, onSurfaceVariant.copy(alpha = 0.25f), CircleShape),
                                        )
                                    } else {
                                        AppCell(entry.icon, entry.label, icon, Modifier.fillMaxWidth()) { open(entry) }
                                    }
                                }
                            }
                        }
                    }
                }
                Spacer(Modifier.height(12.dp))
                Taskbar()
            }
        }
    }

    @Composable
    private fun Taskbar() {
        val view = LocalView.current
        Row(
            Modifier.height(TASKBAR).clip(CircleShape).background(neutral).padding(horizontal = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Row(
                Modifier.fillMaxHeight().padding(vertical = 8.dp).tappable(CircleShape) {
                    view.performHapticFeedback(HapticFeedbackConstants.VIRTUAL_KEY)
                    drawerOpen = true
                }.padding(horizontal = 14.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Icon(painterResource(R.drawable.ic_sl_apps), null, tint = onSurface, modifier = Modifier.size(24.dp))
                Spacer(Modifier.width(10.dp))
                Text("All apps", style = hudText(16.sp, FontWeight.SemiBold, onSurface))
            }
            if (recents.isNotEmpty()) Spacer(Modifier.width(4.dp))
            for (entry in recents) {
                Box(
                    Modifier.size(48.dp).tappable(CircleShape) {
                        view.performHapticFeedback(HapticFeedbackConstants.VIRTUAL_KEY)
                        open(entry)
                    },
                    contentAlignment = Alignment.Center,
                ) {
                    DrawableImage(entry.icon, Modifier.size(36.dp))
                }
            }
        }
    }

    /** An app's icon and name. */
    @Composable
    private fun AppCell(icon: Drawable, label: String, size: Dp, modifier: Modifier = Modifier, onTap: () -> Unit) {
        val view = LocalView.current
        Column(
            modifier
                .tappable(RoundedCornerShape(16.dp)) {
                    view.performHapticFeedback(HapticFeedbackConstants.VIRTUAL_KEY)
                    onTap()
                }
                .padding(vertical = 8.dp, horizontal = 4.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            DrawableImage(icon, Modifier.size(size))
            Spacer(Modifier.height(6.dp))
            Text(
                label,
                style = hudCaption().copy(color = onSurfaceVariant),
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                textAlign = TextAlign.Center,
            )
        }
    }

    /** Every app, on a sheet up over the remote; one picked opens on the head unit, and it closes. */
    @Composable
    private fun AllApps() {
        BackHandler { drawerOpen = false }
        Column(
            Modifier
                .fillMaxSize()
                .statusBarsPadding()
                .padding(top = 8.dp)
                .clip(RoundedCornerShape(topStart = 28.dp, topEnd = 28.dp))
                .background(sheet),
        ) {
            Row(Modifier.fillMaxWidth().padding(8.dp), verticalAlignment = Alignment.CenterVertically) {
                Box(
                    Modifier.size(48.dp).tappable(CircleShape) { drawerOpen = false },
                    contentAlignment = Alignment.Center,
                ) {
                    Icon(painterResource(R.drawable.ic_sl_close), "Close", tint = onSurface, modifier = Modifier.size(24.dp))
                }
                Spacer(Modifier.width(8.dp))
                Text("All apps", style = hudText(22.sp, FontWeight.SemiBold, onSurface))
            }
            val bottom = WindowInsets.navigationBars.asPaddingValues().calculateBottomPadding()
            LazyVerticalGrid(
                columns = GridCells.Adaptive(APP_CELL),
                contentPadding = PaddingValues(start = 12.dp, end = 12.dp, top = 8.dp, bottom = 16.dp + bottom),
                modifier = Modifier.weight(1f).fillMaxWidth(),
            ) {
                items(allApps, key = { it.component.flattenToString() }) { entry ->
                    AppCell(entry.icon, entry.label, APP_ICON) {
                        open(entry)
                        drawerOpen = false
                    }
                }
            }
        }
    }

    /**
     * The title and settings; the launcher, large, over a smaller remote where apps are shown, else
     * the remote, large; then disconnecting.
     */
    @Composable
    private fun Controls() {
        val launcher = showsApps && (connected || preview)
        BoxWithConstraints(
            Modifier.fillMaxSize().background(surface).safeDrawingPadding().padding(24.dp),
        ) {
            if (maxWidth > maxHeight) {
                Row(Modifier.fillMaxSize(), verticalAlignment = Alignment.CenterVertically) {
                    Column(
                        Modifier.weight(1f).fillMaxHeight(),
                        horizontalAlignment = Alignment.CenterHorizontally,
                        verticalArrangement = Arrangement.SpaceBetween,
                    ) {
                        Column(Modifier.weight(1f, fill = false)) {
                            Header(Modifier.fillMaxWidth())
                            if (launcher) {
                                Launcher(Modifier.weight(1f, fill = false).fillMaxWidth().padding(vertical = 16.dp))
                            }
                        }
                        EndSwipe()
                    }
                    Spacer(Modifier.width(24.dp))
                    Remote(Modifier.weight(1f).fillMaxHeight(), compact = false)
                }
            } else {
                Column(
                    Modifier.fillMaxSize(),
                    horizontalAlignment = Alignment.CenterHorizontally,
                    verticalArrangement = Arrangement.SpaceBetween,
                ) {
                    if (launcher) {
                        Header(Modifier.fillMaxWidth().padding(bottom = 16.dp))
                        Launcher(Modifier.weight(1f, fill = false).fillMaxWidth().padding(bottom = 16.dp))
                        Remote(Modifier.fillMaxWidth().padding(bottom = 16.dp), compact = true)
                        EndSwipe()
                    } else {
                        Header(Modifier.fillMaxWidth())
                        Remote(Modifier.weight(1f).fillMaxWidth().padding(vertical = 24.dp), compact = false)
                        EndSwipe()
                    }
                }
            }
        }
    }

    @Composable
    private fun Header(modifier: Modifier) {
        val logo = remember { getDrawable(R.mipmap.ic_sl_launcher) }
        Row(modifier, verticalAlignment = Alignment.CenterVertically) {
            DrawableImage(logo, Modifier.size(44.dp).clip(CircleShape))
            Spacer(Modifier.width(14.dp))
            Column(Modifier.weight(1f)) {
                Text(
                    if (connected || preview) "Connected" else "Not connected",
                    style = hudText(24.sp, FontWeight.SemiBold, onSurface),
                    maxLines = 1,
                )
                Text(
                    if (preview) SlipstreamMode.name(this@PhoneRemoteActivity) else mode,
                    style = hudCaption().copy(color = onSurfaceVariant),
                    maxLines = 1,
                )
            }
            if (!preview) SettingsButton()
        }
    }

    /**
     * The chosen controls: media keys above, the d-pad, back and home below; [compact], under the
     * launcher, back and home, the d-pad and the media keys side by side.
     * Between sessions, where things are up to.
     */
    @Composable
    private fun Remote(modifier: Modifier, compact: Boolean) {
        if (compact) {
            Row(
                modifier,
                horizontalArrangement = Arrangement.SpaceEvenly,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                if (RemoteControl.NAV in controls) {
                    Column(verticalArrangement = Arrangement.spacedBy(16.dp)) {
                        KeyButton(KeyEvent.KEYCODE_BACK, R.drawable.ic_sl_arrow_back, "Back", 52.dp)
                        KeyButton(KEYCODE_HOME, R.drawable.ic_sl_home, "Home", 52.dp)
                    }
                }
                if (RemoteControl.DPAD in controls) {
                    Box(Modifier.size(COMPACT_DPAD)) { DPad() }
                }
                if (RemoteControl.MEDIA in controls) {
                    Column(
                        verticalArrangement = Arrangement.spacedBy(8.dp),
                        horizontalAlignment = Alignment.CenterHorizontally,
                    ) {
                        KeyButton(KeyEvent.KEYCODE_MEDIA_PREVIOUS, R.drawable.ic_sl_skip_previous, "Previous track", 44.dp)
                        KeyButton(
                            KeyEvent.KEYCODE_MEDIA_PLAY_PAUSE, R.drawable.ic_sl_play_pause, "Play or pause", 52.dp,
                            primaryContainer, onPrimaryContainer,
                        )
                        KeyButton(KeyEvent.KEYCODE_MEDIA_NEXT, R.drawable.ic_sl_skip_next, "Next track", 44.dp)
                    }
                }
            }
            return
        }
        Column(
            modifier,
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(28.dp, Alignment.CenterVertically),
        ) {
            if (!connected && !preview) {
                Status()
                return@Column
            }
            if (RemoteControl.MEDIA in controls) {
                Row(horizontalArrangement = Arrangement.spacedBy(20.dp), verticalAlignment = Alignment.CenterVertically) {
                    KeyButton(KeyEvent.KEYCODE_MEDIA_PREVIOUS, R.drawable.ic_sl_skip_previous, "Previous track", 64.dp)
                    KeyButton(
                        KeyEvent.KEYCODE_MEDIA_PLAY_PAUSE, R.drawable.ic_sl_play_pause, "Play or pause", 80.dp,
                        primaryContainer, onPrimaryContainer,
                    )
                    KeyButton(KeyEvent.KEYCODE_MEDIA_NEXT, R.drawable.ic_sl_skip_next, "Next track", 64.dp)
                }
            }
            if (RemoteControl.DPAD in controls) {
                Box(Modifier.weight(1f, fill = false).widthIn(max = 280.dp).aspectRatio(1f)) {
                    DPad()
                }
            }
            if (RemoteControl.NAV in controls) {
                Row(horizontalArrangement = Arrangement.spacedBy(24.dp)) {
                    KeyButton(KeyEvent.KEYCODE_BACK, R.drawable.ic_sl_arrow_back, "Back", 64.dp)
                    KeyButton(KEYCODE_HOME, R.drawable.ic_sl_home, "Home", 64.dp)
                }
            }
        }
    }

    @Composable
    private fun Status() {
        Text(status, style = hudText(26.sp, FontWeight.SemiBold, onSurfaceVariant), textAlign = TextAlign.Center)
        if (retryable) {
            Box(
                Modifier
                    .width(160.dp)
                    .height(64.dp)
                    .tappable(CircleShape) {
                        startService(
                            Intent(this@PhoneRemoteActivity, SlipstreamService::class.java)
                                .setAction(SlipstreamService.ACTION_RETRY),
                        )
                    }
                    .background(primaryContainer),
                contentAlignment = Alignment.Center,
            ) {
                Text("RETRY", style = hudText(20.sp, FontWeight.SemiBold, onPrimaryContainer))
            }
        }
    }

    /** Up, down, left and right round select, each held as long as the finger is. */
    @Composable
    private fun DPad() {
        BoxWithConstraints(Modifier.fillMaxSize().clip(CircleShape).background(neutral)) {
            val button = maxWidth * 0.3f
            val arrow = R.drawable.ic_sl_chevron
            KeyButton(KeyEvent.KEYCODE_DPAD_UP, arrow, "Up", button, neutral, rotation = -90f, modifier = Modifier.align(Alignment.TopCenter))
            KeyButton(KeyEvent.KEYCODE_DPAD_DOWN, arrow, "Down", button, neutral, rotation = 90f, modifier = Modifier.align(Alignment.BottomCenter))
            KeyButton(KeyEvent.KEYCODE_DPAD_LEFT, arrow, "Left", button, neutral, rotation = 180f, modifier = Modifier.align(Alignment.CenterStart))
            KeyButton(KeyEvent.KEYCODE_DPAD_RIGHT, arrow, "Right", button, neutral, modifier = Modifier.align(Alignment.CenterEnd))
            Box(
                Modifier.align(Alignment.Center).size(button).clip(CircleShape).background(primaryContainer)
                    .keyPress(KeyEvent.KEYCODE_DPAD_CENTER, "Select"),
            )
        }
    }

    /** A key held down as long as the finger is, as a head unit's button: select's long press counts. */
    @Composable
    private fun KeyButton(
        code: Int,
        icon: Int,
        description: String,
        size: Dp,
        fill: Color = neutral,
        tint: Color = onSurface,
        rotation: Float = 0f,
        modifier: Modifier = Modifier,
    ) {
        Box(
            modifier.size(size).clip(CircleShape).background(fill).keyPress(code, description),
            contentAlignment = Alignment.Center,
        ) {
            Icon(painterResource(icon), null, tint = tint, modifier = Modifier.size(size * 0.44f).rotate(rotation))
        }
    }

    @Composable
    private fun Modifier.keyPress(code: Int, description: String): Modifier {
        val view = LocalView.current
        var pressed by remember { mutableStateOf(false) }
        return this
            .background(if (pressed) onSurface.copy(alpha = 0.16f) else Color.Transparent)
            .semantics {
                role = Role.Button
                contentDescription = description
                onClick { press(code); true }
            }
            .pointerInput(code) {
                awaitEachGesture {
                    awaitFirstDown().consume()
                    pressed = true
                    view.performHapticFeedback(HapticFeedbackConstants.VIRTUAL_KEY)
                    send(code, true)
                    waitForUpOrCancellation()
                    pressed = false
                    send(code, false)
                }
            }
    }

    /** Slipstream's settings, over this as picture-in-picture. */
    @Composable
    private fun SettingsButton() {
        Box(
            Modifier.size(48.dp).tappable(CircleShape) {
                shrink()
                startActivity(
                    Intent(this@PhoneRemoteActivity, SettingsActivity::class.java)
                        .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                        .putExtra(SettingsActivity.EXTRA_STAY, true),
                )
            }.background(neutral),
            contentAlignment = Alignment.Center,
        ) {
            Icon(painterResource(R.drawable.settings_24), "Slipstream settings", tint = onSurface, modifier = Modifier.size(24.dp))
        }
    }

    /** Disconnects: a dot dragged the length of its track, from its start, which a pocket won't. */
    @Composable
    private fun EndSwipe() {
        var progress by remember { mutableStateOf(0f) }
        Box(
            Modifier
                .width(END_TRACK)
                .height(DOT + DOT_INSET * 2)
                .border(2.dp, tertiary.copy(alpha = 0.6f), CircleShape)
                .pointerInput(Unit) {
                    awaitEachGesture {
                        val start = awaitFirstDown().position
                        if (start.x > (DOT + DOT_INSET * 2 + TRACK_SLOP).toPx()) return@awaitEachGesture
                        while (true) {
                            val change = awaitPointerEvent().changes.firstOrNull() ?: break
                            change.consume()
                            progress = ((change.position.x - start.x) / END_TRAVEL.toPx()).coerceIn(0f, 1f)
                            if (progress == 1f) {
                                end()
                                break
                            }
                            if (!change.pressed) break
                        }
                        progress = 0f
                    }
                },
            contentAlignment = Alignment.CenterStart,
        ) {
            Box(
                Modifier.fillMaxSize().padding(start = DOT + DOT_INSET * 2).alpha(1f - progress),
                contentAlignment = Alignment.Center,
            ) {
                Text("SWIPE TO DISCONNECT", style = hudCaption().copy(color = onSurfaceVariant))
            }
            Box(
                Modifier
                    .padding(start = DOT_INSET)
                    .offset(x = END_TRAVEL * progress)
                    .size(DOT)
                    .background(tertiary, CircleShape),
            )
        }
    }
}

/** The app drawer's cells, and their icons. */
private val APP_CELL = 80.dp
private val APP_ICON = 48.dp
/** The launcher's taskbar, and the d-pad beside it. */
private val TASKBAR = 64.dp
private val COMPACT_DPAD = 168.dp

/** The end swipe's dot, and its inset in the track. */
private val DOT = 40.dp
private val DOT_INSET = 8.dp
/** How far outside the track a swipe may start. */
private val TRACK_SLOP = 24.dp
/** The end swipe's track length, and how far along it the dot travels. */
private val END_TRACK = 280.dp
private val END_TRAVEL = END_TRACK - DOT - DOT_INSET * 2
