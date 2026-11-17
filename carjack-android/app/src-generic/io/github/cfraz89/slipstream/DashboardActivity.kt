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
import android.view.KeyEvent
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.systemBarsPadding
import androidx.compose.foundation.layout.width
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
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import androidx.compose.ui.unit.sp
import io.github.cfraz89.slipstream.launcher.LauncherTheme
import io.github.cfraz89.slipstream.launcher.hudCaption
import io.github.cfraz89.slipstream.launcher.hudText
import io.github.cfraz89.slipstream.launcher.tappable
import java.lang.ref.WeakReference
import java.util.concurrent.Executors

/**
 * The phone's screen in Open mode ([OpenMode]), through head unit sessions and the waits
 * between them: what's on the head unit, with its back and home, and the swipe that disconnects
 * it. An ordinary screen: the head unit's display carries on with the phone locked and its screen
 * off ([VirtualDevices]).
 */
class DashboardActivity : ComponentActivity() {
    companion object {
        /** A session is up, its display on the head unit. */
        var connected by mutableStateOf(false)
        /** Where things are up to, as the service says, and whether it's given up until a retry. */
        var status by mutableStateOf(SlipstreamService.WAITING)
        var retryable by mutableStateOf(false)

        private const val HEAD_UNIT_MS = 2_000L
        private const val ACTION_BACK = "io.github.cfraz89.slipstream.dashboard.BACK"
        private const val ACTION_HOME = "io.github.cfraz89.slipstream.dashboard.HOME"

        private var current = WeakReference<DashboardActivity>(null)

        fun close() {
            current.get()?.let { it.runOnUiThread { it.finish() } }
        }
    }

    private val main = Handler(Looper.getMainLooper())
    private val background = Executors.newSingleThreadExecutor()

    private var headUnitPackage by mutableStateOf<String?>(null)
    private var pip by mutableStateOf(false)

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

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        current = WeakReference(this)
        window.setDecorFitsSystemWindows(false)
        setPictureInPictureParams(pipParams())
        registerReceiver(
            pipAction,
            IntentFilter().apply {
                addAction(ACTION_BACK)
                addAction(ACTION_HOME)
            },
            RECEIVER_NOT_EXPORTED,
        )
        setContent { Dashboard() }
    }

    override fun onStart() {
        super.onStart()
        main.post(pollHeadUnit)
    }

    override fun onStop() {
        main.removeCallbacks(pollHeadUnit)
        super.onStop()
    }

    override fun onDestroy() {
        unregisterReceiver(pipAction)
        background.shutdown()
        super.onDestroy()
    }

    /**
     * Leaving shrinks it to a picture-in-picture window rather than closing it: the head unit's
     * app, with back and home as the window's actions, and expanding it brings the dashboard back.
     */
    private fun pipParams(): PictureInPictureParams = PictureInPictureParams.Builder()
        .setAspectRatio(Rational(1, 1))
        .setAutoEnterEnabled(true)
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
                ACTION_BACK -> background.execute { Variant.key(context, KeyEvent.KEYCODE_BACK) }
                ACTION_HOME -> background.execute { Variant.goHome(context) }
            }
        }
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
        startService(Intent(this, SlipstreamService::class.java).setAction(SlipstreamService.ACTION_DISCONNECT))
        finish()
    }

    private val pollHeadUnit = object : Runnable {
        override fun run() {
            background.execute {
                val top = ShizukuDisplay.topPackage()
                main.post { headUnitPackage = top }
            }
            main.postDelayed(this, HEAD_UNIT_MS)
        }
    }

    private fun appIcon(pkg: String): Drawable? = when (pkg) {
        packageName -> getDrawable(R.drawable.ic_launcher)
        else -> try {
            packageManager.getApplicationIcon(pkg)
        } catch (e: PackageManager.NameNotFoundException) {
            null
        }
    }

    @Composable
    private fun Dashboard() = LauncherTheme {
        Box(Modifier.fillMaxSize().background(Color.Black)) {
            if (pip) {
                Box(Modifier.fillMaxSize().padding(12.dp), contentAlignment = Alignment.Center) {
                    val pkg = headUnitPackage
                    DrawableImage(remember(pkg) { pkg?.let(::appIcon) }, Modifier.fillMaxSize(0.7f))
                }
            } else {
                Controls()
            }
        }
    }

    /**
     * Slipstream's name; what's on the head unit, and its back and home, large, or between
     * sessions where things are up to; Slipstream's settings; then disconnecting.
     */
    @Composable
    private fun Controls() {
        BoxWithConstraints(
            Modifier.fillMaxSize().background(surface).systemBarsPadding().padding(24.dp),
        ) {
            if (maxWidth > maxHeight) {
                Row(Modifier.fillMaxSize(), verticalAlignment = Alignment.CenterVertically) {
                    Column(
                        Modifier.weight(1f).fillMaxHeight(),
                        horizontalAlignment = Alignment.CenterHorizontally,
                        verticalArrangement = Arrangement.SpaceBetween,
                    ) {
                        Header()
                        OnHeadUnit()
                        Spacer(Modifier.height(0.dp))
                    }
                    Column(
                        Modifier.weight(1f),
                        horizontalAlignment = Alignment.CenterHorizontally,
                        verticalArrangement = Arrangement.spacedBy(28.dp),
                    ) {
                        if (connected) HeadUnitButtons(96.dp)
                        SettingsButton()
                        EndSwipe()
                    }
                }
            } else {
                Column(
                    Modifier.fillMaxSize(),
                    horizontalAlignment = Alignment.CenterHorizontally,
                    verticalArrangement = Arrangement.SpaceBetween,
                ) {
                    Header()
                    OnHeadUnit()
                    if (connected) HeadUnitButtons(120.dp)
                    Column(
                        horizontalAlignment = Alignment.CenterHorizontally,
                        verticalArrangement = Arrangement.spacedBy(28.dp),
                    ) {
                        SettingsButton()
                        EndSwipe()
                    }
                }
            }
        }
    }

    @Composable
    private fun Header() {
        val logo = remember { getDrawable(R.mipmap.ic_sl_launcher) }
        Row(verticalAlignment = Alignment.CenterVertically) {
            DrawableImage(logo, Modifier.size(40.dp).clip(CircleShape))
            Spacer(Modifier.width(12.dp))
            Text(
                "SLIPSTREAM",
                style = hudText(26.sp, FontWeight.SemiBold, onSurface).copy(letterSpacing = 0.12.em),
            )
        }
    }

    /** Between sessions, where things are up to; nothing while the head unit's app isn't known yet. */
    @Composable
    private fun OnHeadUnit() {
        if (!connected) {
            Column(horizontalAlignment = Alignment.CenterHorizontally) {
                Text(status, style = hudText(26.sp, FontWeight.SemiBold, onSurfaceVariant), textAlign = TextAlign.Center)
                if (retryable) {
                    Spacer(Modifier.height(16.dp))
                    Box(
                        Modifier
                            .width(160.dp)
                            .height(64.dp)
                            .tappable(CircleShape) {
                                startService(
                                    Intent(this@DashboardActivity, SlipstreamService::class.java)
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
            return
        }
        val pkg = headUnitPackage ?: return
        val icon = remember(pkg) { appIcon(pkg) }
        val label = remember(pkg) { appLabel(pkg) }
        Column(horizontalAlignment = Alignment.CenterHorizontally) {
            DrawableImage(icon, Modifier.size(96.dp))
            Spacer(Modifier.height(16.dp))
            Text(label, style = hudText(34.sp, FontWeight.SemiBold, onSurface), maxLines = 1)
            Text("ON HEAD UNIT", style = hudCaption().copy(color = onSurfaceVariant))
        }
    }

    private fun appLabel(pkg: String): String = when (pkg) {
        packageName -> "Launcher"
        else -> try {
            packageManager.getApplicationLabel(packageManager.getApplicationInfo(pkg, 0)).toString()
        } catch (e: PackageManager.NameNotFoundException) {
            pkg
        }
    }

    /** Back and home on the head unit, and its launcher where home is another app. */
    @Composable
    private fun HeadUnitButtons(size: Dp) {
        Row(horizontalArrangement = Arrangement.spacedBy(24.dp)) {
            RoundButton(R.drawable.ic_sl_arrow_back, "Back", size, primaryContainer, onPrimaryContainer) {
                background.execute { Variant.key(this@DashboardActivity, KeyEvent.KEYCODE_BACK) }
            }
            RoundButton(R.drawable.ic_sl_home, "Home", size, primaryContainer, onPrimaryContainer) {
                background.execute { Variant.goHome(this@DashboardActivity) }
            }
            if (!Variant.opensLauncher(this@DashboardActivity)) {
                RoundButton(R.drawable.ic_apps, "Launcher", size, primaryContainer, onPrimaryContainer) {
                    background.execute { Variant.openLauncher() }
                }
            }
        }
    }

    /** Slipstream's settings, over this as picture-in-picture. */
    @Composable
    private fun SettingsButton() {
        RoundButton(R.drawable.settings_24, "Slipstream settings", 56.dp) {
            shrink()
            startActivity(Intent(this@DashboardActivity, SettingsActivity::class.java))
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

    @Composable
    private fun RoundButton(
        icon: Int,
        description: String,
        size: Dp,
        fill: Color = neutral,
        tint: Color = onSurface,
        onClick: () -> Unit,
    ) {
        Box(
            Modifier.size(size).tappable(CircleShape, onClick = onClick).background(fill),
            contentAlignment = Alignment.Center,
        ) {
            Icon(painterResource(icon), description, tint = tint, modifier = Modifier.size(size * 0.44f))
        }
    }
}

/** The end swipe's dot, and its inset in the track. */
private val DOT = 40.dp
private val DOT_INSET = 8.dp
/** How far outside the track a swipe may start. */
private val TRACK_SLOP = 24.dp
/** The end swipe's track length, and how far along it the dot travels. */
private val END_TRACK = 280.dp
private val END_TRAVEL = END_TRACK - DOT - DOT_INSET * 2
