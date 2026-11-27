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
import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.AnimatedVisibilityScope
import androidx.compose.animation.ExperimentalSharedTransitionApi
import androidx.compose.animation.SharedTransitionLayout
import androidx.compose.animation.SharedTransitionScope
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.tween
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.slideOutVertically
import androidx.compose.animation.togetherWith
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.withFrameNanos
import androidx.compose.ui.graphics.drawscope.Stroke
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
import androidx.compose.ui.layout.layout
import androidx.compose.ui.unit.Constraints
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
 * The app's own screen. Waiting, where the connection's up to, and what's left to set up; once a
 * session's up, in either mode, a remote for the head unit - the controls chosen in settings
 * ([RemoteControl]), sending what its switchgear would - and the swipe that disconnects. It keeps
 * the phone awake while it's showing, so it can be opened ahead of the head unit; the head unit
 * carries on with the phone locked and its screen off all the same.
 */
class PhoneRemoteActivity : ComponentActivity() {
    companion object {
        /** A session is up, its display on the head unit. */
        var connected by mutableStateOf(false)
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

        /** At a session's end; waiting for one, it stays. */
        fun close() {
            current.get()?.let { it.runOnUiThread { if (it.shownRemote) it.finish() } }
        }
    }

    /** The remote's been up: it stays, through a mode change's moment without a display, until it closes. */
    private var shownRemote by mutableStateOf(false)
    private var listening by mutableStateOf(true)
    private var issues by mutableStateOf(emptyList<SetupIssue>())

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
    private val primary by lazy { tone(R.color.sl_primary) }
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
        SlipstreamMode.listen(this)
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
        .setAutoEnterEnabled(connected && !preview)
        .setActions(
            listOf(
                pipAction(ACTION_BACK, R.drawable.ic_sl_nav_back, "Back"),
                pipAction(ACTION_HOME, R.drawable.ic_sl_nav_home, "Home"),
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

    /**
     * What's on the head unit in Slipstream mode, for the picture-in-picture window; and, waiting,
     * whether Slipstream's on and what's left to set up.
     */
    private val pollHeadUnit = object : Runnable {
        override fun run() {
            background.execute {
                val top = ShizukuDisplay.topPackage()
                val on = SlipstreamMode.isListening(this@PhoneRemoteActivity)
                val found = SetupIssue.of(this@PhoneRemoteActivity)
                main.post {
                    headUnitPackage = top
                    listening = on
                    issues = found
                }
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

    @OptIn(ExperimentalSharedTransitionApi::class)
    @Composable
    private fun PhoneRemote() = LauncherTheme {
        Box(Modifier.fillMaxSize().background(Color.Black)) {
            if (pip) {
                Box(Modifier.fillMaxSize().padding(12.dp), contentAlignment = Alignment.Center) {
                    val pkg = headUnitPackage
                    DrawableImage(remember(pkg) { appIcon(pkg) }, Modifier.fillMaxSize(0.7f))
                }
            } else {
                LaunchedEffect(connected) {
                    if (connected) shownRemote = true
                    setPictureInPictureParams(pipParams())
                }
                SharedTransitionLayout {
                    AnimatedContent(
                        connected || preview || shownRemote,
                        transitionSpec = { fadeIn(tween(SCREEN_MS)) togetherWith fadeOut(tween(SCREEN_MS)) },
                        label = "screen",
                    ) { remote ->
                        val shared = Shared(this@SharedTransitionLayout, this)
                        if (remote) Controls(shared) else Connect(shared)
                    }
                }
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
        BoxWithConstraints(modifier, contentAlignment = Alignment.Center) {
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
     * The title and settings; the launcher, large, beside or over a smaller remote where apps are
     * shown, else the remote, large; then disconnecting, below, or at the side in landscape.
     */
    @Composable
    private fun Controls(shared: Shared) {
        val launcher = showsApps
        BoxWithConstraints(
            Modifier.fillMaxSize().background(surface).safeDrawingPadding().padding(24.dp),
        ) {
            if (maxWidth > maxHeight) {
                Column(Modifier.fillMaxSize()) {
                    Header(shared, Modifier.fillMaxWidth().padding(bottom = 16.dp))
                    Row(Modifier.weight(1f).fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                        if (launcher) {
                            Launcher(Modifier.weight(2f).fillMaxHeight())
                            Spacer(Modifier.width(24.dp))
                            Remote(Modifier.weight(1f).fillMaxHeight(), RemoteLayout.SIDE)
                        } else {
                            Remote(Modifier.weight(1f).fillMaxHeight(), RemoteLayout.WIDE)
                        }
                        Spacer(Modifier.width(24.dp))
                        BoxWithConstraints(Modifier.fillMaxHeight(), contentAlignment = Alignment.Center) {
                            EndSwipe(minOf(maxHeight, END_TRACK), vertical = true)
                        }
                    }
                }
            } else {
                Column(
                    Modifier.fillMaxSize(),
                    horizontalAlignment = Alignment.CenterHorizontally,
                    verticalArrangement = Arrangement.SpaceBetween,
                ) {
                    if (launcher) {
                        Header(shared, Modifier.fillMaxWidth().padding(bottom = 16.dp))
                        Launcher(Modifier.weight(1f, fill = false).fillMaxWidth().padding(bottom = 16.dp))
                        Remote(Modifier.fillMaxWidth().padding(bottom = 16.dp), RemoteLayout.ROW)
                        EndSwipe()
                    } else {
                        Header(shared, Modifier.fillMaxWidth())
                        Remote(Modifier.weight(1f).fillMaxWidth().padding(vertical = 24.dp), RemoteLayout.COLUMN)
                        EndSwipe()
                    }
                }
            }
        }
    }

    @Composable
    private fun Header(shared: Shared, modifier: Modifier) {
        val logo = remember { getDrawable(R.mipmap.ic_sl_launcher) }
        Row(modifier, verticalAlignment = Alignment.CenterVertically) {
            DrawableImage(logo, Modifier.shared(shared, LOGO).size(44.dp).clip(CircleShape))
            Spacer(Modifier.width(14.dp))
            Column(Modifier.weight(1f)) {
                Title(shared)
                Text(
                    "Connected",
                    style = hudCaption().copy(color = onSurfaceVariant),
                    maxLines = 1,
                    modifier = Modifier.shared(shared, STATUS),
                )
            }
            if (!preview) SettingsButton()
        }
    }

    @Composable
    private fun Title(shared: Shared) {
        Text(
            "Slipstream",
            style = hudText(24.sp, FontWeight.SemiBold, onSurface),
            maxLines = 1,
            modifier = Modifier.shared(shared, TITLE),
        )
    }

    /**
     * Waiting for the head unit: the title and settings, and in the middle a calm pulse round the
     * logo while Slipstream listens, quickening while it connects, with where things are up to;
     * what's left to set up, and Slipstream turned off, with what sees to them.
     */
    @Composable
    private fun Connect(shared: Shared) {
        val wait = when {
            !listening -> Wait.OFF
            retryable -> Wait.FAILED
            status == SlipstreamService.WAITING || status.startsWith("Session ended") -> Wait.WAITING
            else -> Wait.CONNECTING
        }
        Column(Modifier.fillMaxSize().background(surface).safeDrawingPadding().padding(24.dp)) {
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Box(Modifier.weight(1f)) { Title(shared) }
                SettingsButton()
            }
            BoxWithConstraints(Modifier.weight(1f).fillMaxWidth(), contentAlignment = Alignment.Center) {
                if (maxWidth > maxHeight) {
                    Row(Modifier.fillMaxSize(), verticalAlignment = Alignment.CenterVertically) {
                        Pulse(wait, shared, Modifier.weight(1f).fillMaxHeight())
                        Spacer(Modifier.width(24.dp))
                        Column(
                            Modifier.weight(1f).fillMaxHeight().verticalScroll(rememberScrollState()),
                            horizontalAlignment = Alignment.CenterHorizontally,
                            verticalArrangement = Arrangement.Center,
                        ) {
                            Waiting(wait, shared)
                        }
                    }
                } else {
                    Column(
                        Modifier.fillMaxSize().verticalScroll(rememberScrollState()),
                        horizontalAlignment = Alignment.CenterHorizontally,
                        verticalArrangement = Arrangement.Center,
                    ) {
                        Pulse(wait, shared, Modifier.fillMaxWidth().height(PULSE))
                        Spacer(Modifier.height(24.dp))
                        Waiting(wait, shared)
                    }
                }
            }
        }
    }

    /** Where things are up to, and what to do about it. */
    @Composable
    private fun Waiting(wait: Wait, shared: Shared) {
        val headline = when {
            wait == Wait.OFF -> "Slipstream is off"
            status != SlipstreamService.WAITING -> status
            issues.isNotEmpty() -> "Setup needed"
            else -> "Ready to connect"
        }
        Text(
            headline,
            style = hudText(if (headline.length > 40) 18.sp else 24.sp, FontWeight.SemiBold, onSurface),
            textAlign = TextAlign.Center,
            modifier = Modifier.shared(shared, STATUS),
        )
        val hint = when {
            wait == Wait.OFF -> "It won't connect to the head unit until it's turned on"
            wait == Wait.WAITING && issues.isEmpty() -> "Turn on the head unit and accept CarPlay"
            else -> null
        }
        if (hint != null) {
            Spacer(Modifier.height(8.dp))
            Text(hint, style = hudCaption().copy(color = onSurfaceVariant), textAlign = TextAlign.Center)
        }
        when (wait) {
            Wait.OFF -> Pill("Turn on") {
                SlipstreamMode.setListening(this@PhoneRemoteActivity, true)
                SlipstreamMode.listen(this@PhoneRemoteActivity)
                listening = true
            }
            Wait.FAILED -> Pill("Retry") {
                startService(
                    Intent(this@PhoneRemoteActivity, SlipstreamService::class.java)
                        .setAction(SlipstreamService.ACTION_RETRY),
                )
            }
            else -> {}
        }
        if (issues.isNotEmpty()) {
            Spacer(Modifier.height(24.dp))
            Column(
                Modifier.widthIn(max = 420.dp).fillMaxWidth().clip(RoundedCornerShape(20.dp)).background(sheet)
                    .padding(16.dp),
            ) {
                for (issue in issues) {
                    Row(Modifier.padding(vertical = 6.dp), verticalAlignment = Alignment.CenterVertically) {
                        Icon(painterResource(R.drawable.ic_sl_info), null, tint = tertiary, modifier = Modifier.size(20.dp))
                        Spacer(Modifier.width(12.dp))
                        Text(issue.text, style = hudText(15.sp, FontWeight.Normal, onSurface))
                    }
                }
                Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.CenterEnd) {
                    Pill("Settings", ::openSettings)
                }
            }
        }
    }

    @Composable
    private fun Pill(label: String, onTap: () -> Unit) {
        Box(
            Modifier
                .padding(top = 16.dp)
                .height(52.dp)
                .widthIn(min = 140.dp)
                .tappable(CircleShape, onClick = onTap)
                .background(primaryContainer)
                .padding(horizontal = 24.dp),
            contentAlignment = Alignment.Center,
        ) {
            Text(label.uppercase(), style = hudText(17.sp, FontWeight.SemiBold, onPrimaryContainer))
        }
    }

    /** Rings spreading from the logo: slow while listening, quick while connecting, gone otherwise. */
    @Composable
    private fun Pulse(wait: Wait, shared: Shared, modifier: Modifier) {
        val logo = remember { getDrawable(R.mipmap.ic_sl_launcher) }
        val speed by animateFloatAsState(
            when (wait) {
                Wait.WAITING -> 1f
                Wait.CONNECTING -> 3f
                else -> 0f
            },
            tween(PULSE_EASE_MS),
            label = "speed",
        )
        val strength by animateFloatAsState(
            when (wait) {
                Wait.WAITING -> 0.5f
                Wait.CONNECTING -> 1f
                else -> 0f
            },
            tween(PULSE_EASE_MS),
            label = "strength",
        )
        var phase by remember { mutableFloatStateOf(0f) }
        LaunchedEffect(Unit) {
            var last = 0L
            while (true) {
                withFrameNanos { now ->
                    if (last != 0L) phase = (phase + (now - last) / 1e9f * speed / RING_PERIOD_S) % 1f
                    last = now
                }
            }
        }
        Box(modifier, contentAlignment = Alignment.Center) {
            Canvas(Modifier.fillMaxSize()) {
                val inner = PULSE_LOGO.toPx() / 2
                val outer = size.minDimension / 2
                for (i in 0 until RINGS) {
                    val t = (phase + i.toFloat() / RINGS) % 1f
                    drawCircle(
                        primary,
                        radius = inner + (outer - inner) * t,
                        alpha = (1f - t) * strength * 0.6f,
                        style = Stroke(2.dp.toPx()),
                    )
                }
            }
            DrawableImage(
                logo,
                Modifier.shared(shared, LOGO).size(PULSE_LOGO).clip(CircleShape)
                    .alpha(if (wait == Wait.OFF) 0.5f else 1f),
            )
        }
    }

    private fun openSettings() {
        if (connected) shrink()
        startActivity(Intent(this, SettingsActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    }

    /**
     * The chosen controls. [RemoteLayout.COLUMN]: media keys above, the d-pad, back and home
     * below; [RemoteLayout.SIDE] the same, smaller, beside the launcher; [RemoteLayout.ROW], under
     * the launcher, the d-pad and the media keys side by side, back and home below;
     * [RemoteLayout.WIDE], landscape, back and home, the d-pad and the media keys side by side.
     */
    @Composable
    private fun Remote(modifier: Modifier, layout: RemoteLayout) {
        if (layout == RemoteLayout.SIDE) {
            Column(
                modifier,
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(16.dp, Alignment.CenterVertically),
            ) {
                if (RemoteControl.MEDIA in controls) {
                    Row(horizontalArrangement = Arrangement.spacedBy(12.dp), verticalAlignment = Alignment.CenterVertically) {
                        KeyButton(KeyEvent.KEYCODE_MEDIA_PREVIOUS, R.drawable.ic_sl_skip_previous, "Previous track", 44.dp)
                        KeyButton(
                            KeyEvent.KEYCODE_MEDIA_PLAY_PAUSE, R.drawable.ic_sl_play_pause, "Play or pause", 56.dp,
                            primaryContainer, onPrimaryContainer,
                        )
                        KeyButton(KeyEvent.KEYCODE_MEDIA_NEXT, R.drawable.ic_sl_skip_next, "Next track", 44.dp)
                    }
                }
                if (RemoteControl.DPAD in controls) {
                    Box(Modifier.weight(1f, fill = false).widthIn(max = 220.dp).aspectRatio(1f)) { DPad() }
                }
                if (RemoteControl.NAV in controls) NavButtons(60.dp, 20.dp)
            }
            return
        }
        if (layout == RemoteLayout.ROW || layout == RemoteLayout.WIDE) {
            val media: @Composable () -> Unit = {
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
            if (layout == RemoteLayout.WIDE) {
                Row(
                    modifier,
                    horizontalArrangement = Arrangement.SpaceEvenly,
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    if (RemoteControl.NAV in controls) NavButtons(72.dp, 24.dp)
                    if (RemoteControl.DPAD in controls) {
                        Box(Modifier.weight(1f, fill = false).widthIn(max = 220.dp).aspectRatio(1f)) { DPad() }
                    }
                    if (RemoteControl.MEDIA in controls) media()
                }
                return
            }
            Column(
                modifier,
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(16.dp),
            ) {
                if (RemoteControl.DPAD in controls || RemoteControl.MEDIA in controls) {
                    Row(
                        Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.SpaceEvenly,
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        if (RemoteControl.DPAD in controls) Box(Modifier.size(COMPACT_DPAD)) { DPad() }
                        if (RemoteControl.MEDIA in controls) media()
                    }
                }
                if (RemoteControl.NAV in controls) NavButtons(64.dp, 32.dp)
            }
            return
        }
        Column(
            modifier,
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(28.dp, Alignment.CenterVertically),
        ) {
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
            if (RemoteControl.NAV in controls) NavButtons(72.dp, 32.dp)
        }
    }

    /** Back and home, side by side, as the head unit's system bar draws them. */
    @Composable
    private fun NavButtons(size: Dp, gap: Dp) {
        Row(horizontalArrangement = Arrangement.spacedBy(gap)) {
            KeyButton(KeyEvent.KEYCODE_BACK, R.drawable.ic_sl_nav_back, "Back", size)
            KeyButton(KEYCODE_HOME, R.drawable.ic_sl_nav_home, "Home", size)
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

    /** Slipstream's settings; over this as picture-in-picture during a session. */
    @Composable
    private fun SettingsButton() {
        Box(
            Modifier.size(48.dp).tappable(CircleShape, onClick = ::openSettings).background(neutral),
            contentAlignment = Alignment.Center,
        ) {
            Icon(painterResource(R.drawable.settings_24), "Slipstream settings", tint = onSurface, modifier = Modifier.size(24.dp))
        }
    }

    /**
     * Disconnects: a dot dragged the length of its track, from its start, which a pocket won't.
     * Left to right, or [vertical], bottom to top.
     */
    @Composable
    private fun EndSwipe(track: Dp = END_TRACK, vertical: Boolean = false) {
        var progress by remember { mutableStateOf(0f) }
        val travel = track - DOT - DOT_INSET * 2
        val thickness = DOT + DOT_INSET * 2
        Box(
            Modifier
                .size(if (vertical) thickness else track, if (vertical) track else thickness)
                .border(2.dp, tertiary.copy(alpha = 0.6f), CircleShape)
                .pointerInput(travel, vertical) {
                    awaitEachGesture {
                        val start = awaitFirstDown().position
                        val reach = (thickness + TRACK_SLOP).toPx()
                        if (if (vertical) start.y < size.height - reach else start.x > reach) return@awaitEachGesture
                        while (true) {
                            val change = awaitPointerEvent().changes.firstOrNull() ?: break
                            change.consume()
                            val moved = if (vertical) start.y - change.position.y else change.position.x - start.x
                            progress = (moved / travel.toPx()).coerceIn(0f, 1f)
                            if (progress == 1f) {
                                end()
                                break
                            }
                            if (!change.pressed) break
                        }
                        progress = 0f
                    }
                },
            contentAlignment = if (vertical) Alignment.BottomCenter else Alignment.CenterStart,
        ) {
            Box(
                Modifier.fillMaxSize()
                    .padding(start = if (vertical) 0.dp else thickness, bottom = if (vertical) thickness else 0.dp)
                    .alpha(1f - progress),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    "SWIPE TO DISCONNECT",
                    style = hudCaption().copy(color = onSurfaceVariant),
                    maxLines = 1,
                    modifier = if (vertical) Modifier.readingUp() else Modifier,
                )
            }
            Box(
                Modifier
                    .padding(start = if (vertical) 0.dp else DOT_INSET, bottom = if (vertical) DOT_INSET else 0.dp)
                    .offset(x = if (vertical) 0.dp else travel * progress, y = if (vertical) -travel * progress else 0.dp)
                    .size(DOT)
                    .background(tertiary, CircleShape),
            )
        }
    }

    /** Turned a quarter anticlockwise, laid out at its turned size: read bottom to top. */
    private fun Modifier.readingUp(): Modifier = layout { measurable, constraints ->
        val placeable = measurable.measure(
            Constraints(maxWidth = constraints.maxHeight, maxHeight = constraints.maxWidth),
        )
        layout(placeable.height, placeable.width) {
            val shift = (placeable.width - placeable.height) / 2
            placeable.placeWithLayer(-shift, shift) { rotationZ = -90f }
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
/** The end swipe's track length, at most. */
private val END_TRACK = 280.dp

/** The waiting screen's pulse, the logo in its middle, and how long a ring takes to spread at the slow pace. */
private val PULSE = 280.dp
private val PULSE_LOGO = 96.dp
private const val RINGS = 3
private const val RING_PERIOD_S = 3.6f
private const val PULSE_EASE_MS = 800
/** The waiting screen giving way to the remote. */
private const val SCREEN_MS = 500

/** The shared elements' keys, between the waiting screen and the remote. */
private const val LOGO = "logo"
private const val TITLE = "title"
private const val STATUS = "status"

private enum class Wait { OFF, WAITING, CONNECTING, FAILED }

private enum class RemoteLayout { COLUMN, ROW, SIDE, WIDE }

/** What an element shared between the waiting screen and the remote moves within. */
@OptIn(ExperimentalSharedTransitionApi::class)
private class Shared(val scope: SharedTransitionScope, val visibility: AnimatedVisibilityScope)

@OptIn(ExperimentalSharedTransitionApi::class)
@Composable
private fun Modifier.shared(shared: Shared, key: String): Modifier = with(shared.scope) {
    this@shared.sharedBounds(rememberSharedContentState(key), shared.visibility)
}
