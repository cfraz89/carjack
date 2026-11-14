package io.github.cfraz89.slipstream

import android.app.KeyguardManager
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
import android.hardware.Sensor
import android.hardware.SensorEvent
import android.hardware.SensorEventListener
import android.hardware.SensorManager
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import android.util.Rational
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.WindowInsets
import android.view.WindowInsetsController
import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.activity.addCallback
import androidx.activity.compose.setContent
import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
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
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.boundsInRoot
import androidx.compose.ui.layout.onGloballyPositioned
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
 * The phone's screen while activated ([Activation]), through head unit sessions and the waits
 * between them: what's on the head unit, with its back and home, and the swipe that ends it all
 * (to the user, the session). Put to sleep by its
 * button or by a pocket, it's black for the least power, like the phone's own lock screen - a tap
 * lights it a moment, and a swipe up wakes it; in the pocket, a tap can't light it. It keeps the
 * screen on, so the phone neither sleeps (blanking the head unit) nor locks. Locked anyway, by the
 * power button, the swipe up unlocks.
 */
class DashboardActivity : ComponentActivity() {
    companion object {
        /** A session is up, its display on the head unit. */
        var connected by mutableStateOf(false)
        /** Where things are up to, as the service says, and whether it's given up until a retry. */
        var status by mutableStateOf(SlipstreamService.WAITING)
        var retryable by mutableStateOf(false)

        private const val HEAD_UNIT_MS = 2_000L
        /** Near for this long, untouched, before it goes black: a hand reaching for it isn't a pocket. */
        private const val COVER_MS = 1_500L
        /** Uncovered for this long before it lights again: a pocket's jostling isn't uncovering. */
        private const val UNCOVER_MS = 1_000L
        private const val LOCK_POLL_MS = 500L
        /** How long a tap lights the sleeping screen, from the last touch. */
        private const val PEEK_MS = 4_000L
        private const val ACTION_BACK = "io.github.cfraz89.slipstream.dashboard.BACK"
        private const val ACTION_HOME = "io.github.cfraz89.slipstream.dashboard.HOME"

        private var current = WeakReference<DashboardActivity>(null)

        fun close() {
            current.get()?.let { it.runOnUiThread { it.finish() } }
        }
    }

    /** How the window is set: lit, lit a moment over the sleeping screen, or black. */
    private enum class Shown { LIT, PEEK, BLACK }

    private val main = Handler(Looper.getMainLooper())
    private val background = Executors.newSingleThreadExecutor()

    /** In a pocket, by the proximity sensor. */
    private var covered by mutableStateOf(false)
    /** Asleep: put to sleep by its button, a pocket or the lock screen. */
    private var asleep by mutableStateOf(false)
    /** Lit a moment by a tap, asleep still. */
    private var peeking by mutableStateOf(false)
    /** Shown over the lock screen: waking means unlocking first. */
    private var locked by mutableStateOf(false)
    /** Where the swipe hint's track is, in the window, once it's been shown. */
    private var track: Rect? = null
    /** How far up the track a swipe has the dot, while one's under way. */
    private var drag by mutableStateOf<Float?>(null)
    private var shown: Shown? = null
    private var started = false
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
    private val primary by lazy { tone(R.color.sl_primary) }
    private val onPrimary by lazy { tone(R.color.sl_on_primary) }
    private val primaryContainer by lazy { tone(R.color.sl_primary_container) }
    private val onPrimaryContainer by lazy { tone(R.color.sl_on_primary_container) }
    private val tertiary by lazy { tone(R.color.sl_tertiary) }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        current = WeakReference(this)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
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
        // Leaving is home or picture-in-picture, never back: without this screen the phone sleeps.
        onBackPressedDispatcher.addCallback(this) { sleep(true) }
        setContent { Dashboard() }
    }

    override fun onStart() {
        super.onStart()
        started = true
        val now = keyguard().isKeyguardLocked
        if (locked && !now) asleep = false
        locked = now
        if (locked) asleep = true
        sensors()?.let { sm ->
            sm.getDefaultSensor(Sensor.TYPE_PROXIMITY)?.let {
                sm.registerListener(proximity, it, SensorManager.SENSOR_DELAY_NORMAL)
            }
        }
        shown = null
        applyShown()
    }

    override fun onStop() {
        started = false
        sensors()?.unregisterListener(proximity)
        main.removeCallbacks(pollHeadUnit)
        main.removeCallbacks(uncover)
        main.removeCallbacks(covering)
        main.removeCallbacks(endPeek)
        main.removeCallbacks(watchLock)
        // Stopped by the screen going off: taken as locked, so it wakes if it comes back unlocked,
        // the keyguard often gone already (an unlock from the screen off) by onStart.
        if (!getSystemService(PowerManager::class.java).isInteractive) locked = true
        covered = false
        peeking = false
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

    /** As a picture-in-picture window it's lit, and comes back from one awake. */
    override fun onPictureInPictureModeChanged(inPip: Boolean, newConfig: Configuration) {
        super.onPictureInPictureModeChanged(inPip, newConfig)
        pip = inPip
        if (inPip) {
            covered = false
            asleep = false
            peeking = false
        }
        applyShown()
    }

    /** Out of the way as a picture-in-picture window, the phone's own screen showing beneath. */
    private fun shrink(): Boolean = try {
        enterPictureInPictureMode(pipParams())
    } catch (e: IllegalStateException) {
        false
    }

    private fun sensors() = getSystemService(SensorManager::class.java)

    private fun keyguard() = getSystemService(KeyguardManager::class.java)

    /**
     * While asleep, the lock screen watched: unlocked, however it was, it wakes, unless it's in a
     * pocket. Polled: the system's unlock broadcast doesn't come for an unlock over this screen.
     */
    private val watchLock = object : Runnable {
        override fun run() {
            val now = keyguard().isKeyguardLocked
            if (locked && !now && !covered) sleep(false)
            locked = now
            if (started && asleep) main.postDelayed(this, LOCK_POLL_MS)
        }
    }

    /** Out of sleep, by way of the phone's own unlock where it's locked. */
    private fun wake() {
        if (!keyguard().isKeyguardLocked) {
            locked = false
            sleep(false)
            return
        }
        keyguard().requestDismissKeyguard(
            this,
            object : KeyguardManager.KeyguardDismissCallback() {
                override fun onDismissSucceeded() {
                    locked = false
                    sleep(false)
                }
            },
        )
    }

    /** All over: the service stopped, and this screen with it. */
    private fun end() {
        Activation.stop(this)
        finish()
    }

    private val proximity = object : SensorEventListener {
        override fun onSensorChanged(event: SensorEvent) {
            val near = event.values[0] < event.sensor.maximumRange
            main.removeCallbacks(uncover)
            main.removeCallbacks(covering)
            if (near) {
                if (!covered) main.postDelayed(covering, COVER_MS)
            } else if (covered) {
                main.postDelayed(uncover, UNCOVER_MS)
            }
        }

        override fun onAccuracyChanged(sensor: Sensor, accuracy: Int) {}
    }

    private val covering = Runnable { cover(true) }
    private val uncover = Runnable { cover(false) }
    private val endPeek = Runnable {
        peeking = false
        applyShown()
    }

    override fun dispatchTouchEvent(event: MotionEvent): Boolean {
        if (event.actionMasked == MotionEvent.ACTION_DOWN && shown == Shown.LIT) main.removeCallbacks(covering)
        return super.dispatchTouchEvent(event)
    }

    /**
     * Covered, it goes to sleep, and stays asleep uncovered, lit a moment as by a tap: out of a
     * pocket takes a swipe.
     */
    private fun cover(value: Boolean) {
        if (covered == value || (value && pip)) return
        covered = value
        if (value) {
            peeking = false
            asleep = true
        } else if (asleep) {
            peek()
        }
        applyShown()
    }

    private fun sleep(value: Boolean) {
        main.removeCallbacks(endPeek)
        peeking = false
        if (asleep == value) return
        asleep = value
        applyShown()
    }

    /** Lit a moment longer, unless it's in a pocket. */
    private fun peek() {
        if (covered) return
        main.removeCallbacks(endPeek)
        main.postDelayed(endPeek, PEEK_MS)
        if (peeking) return
        peeking = true
        applyShown()
    }

    /**
     * Black: dimmed right down, at the display's slowest refresh, the system bars away so a pocket
     * can't swipe them open, and no asking after the head unit. Peeking: lit, for the swipe's
     * animation, the bars still away. Lit: all of it back.
     */
    private fun applyShown() {
        main.removeCallbacks(watchLock)
        if (started && asleep) main.postDelayed(watchLock, LOCK_POLL_MS)
        val value = when {
            pip || !(covered || asleep) -> Shown.LIT
            peeking -> Shown.PEEK
            else -> Shown.BLACK
        }
        if (shown == value) return
        shown = value
        window.attributes = window.attributes.apply {
            screenBrightness = if (value == Shown.BLACK) 0f else WindowManager.LayoutParams.BRIGHTNESS_OVERRIDE_NONE
            preferredDisplayModeId = if (value == Shown.BLACK) slowestMode() else 0
        }
        window.insetsController?.let {
            if (value == Shown.LIT) {
                it.show(WindowInsets.Type.systemBars())
            } else {
                it.systemBarsBehavior = WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
                it.hide(WindowInsets.Type.systemBars())
            }
        }
        main.removeCallbacks(pollHeadUnit)
        if (value == Shown.LIT && started) main.post(pollHeadUnit)
    }

    /** The display's slowest mode at its current size, or 0 for no preference. */
    private fun slowestMode(): Int {
        val display = display ?: return 0
        val now = display.mode
        return display.supportedModes
            .filter { it.physicalWidth == now.physicalWidth && it.physicalHeight == now.physicalHeight }
            .minByOrNull { it.refreshRate }?.modeId ?: 0
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
            when {
                pip -> Box(Modifier.fillMaxSize().padding(12.dp), contentAlignment = Alignment.Center) {
                    val pkg = headUnitPackage
                    DrawableImage(remember(pkg) { pkg?.let(::appIcon) }, Modifier.fillMaxSize(0.7f))
                }
                covered || asleep -> Asleep()
                else -> Controls()
            }
        }
    }

    /**
     * Black, every touch taken: a touch lights it a moment, and, lit, a swipe up the hint's track
     * wakes it, the dot following the finger.
     */
    @Composable
    private fun Asleep() {
        Box(
            Modifier.fillMaxSize().pointerInput(Unit) {
                awaitEachGesture {
                    val start = awaitFirstDown(requireUnconsumed = false).position
                    val onTrack = peeking && track?.inflate(TRACK_SLOP.toPx())?.contains(start) == true
                    peek()
                    while (true) {
                        val change = awaitPointerEvent().changes.firstOrNull() ?: break
                        change.consume()
                        if (onTrack) {
                            val up = ((start.y - change.position.y) / DOT_TRAVEL.toPx()).coerceIn(0f, 1f)
                            drag = up
                            if (up == 1f && !covered) {
                                wake()
                                break
                            }
                        }
                        if (!change.pressed) break
                    }
                    drag = null
                }
            },
            contentAlignment = Alignment.BottomCenter,
        ) {
            if (peeking) {
                Column(Modifier.padding(bottom = 72.dp), horizontalAlignment = Alignment.CenterHorizontally) {
                    Text(
                        if (connected) "ON HEAD UNIT" else status,
                        style = hudCaption().copy(color = onSurfaceVariant),
                        textAlign = TextAlign.Center,
                        modifier = Modifier.padding(horizontal = 24.dp),
                    )
                    Spacer(Modifier.height(32.dp))
                    SwipeHint()
                }
            }
        }
    }

    /** A track with a dot rising up it, or following a swipe, over "swipe up". */
    @Composable
    private fun SwipeHint(modifier: Modifier = Modifier) {
        val rise by rememberInfiniteTransition(label = "swipe").animateFloat(
            initialValue = 0f,
            targetValue = 1f,
            animationSpec = infiniteRepeatable(tween(1_400, easing = LinearEasing), RepeatMode.Restart),
            label = "rise",
        )
        val up = drag ?: rise
        Column(modifier, horizontalAlignment = Alignment.CenterHorizontally) {
            Box(
                Modifier
                    .width(DOT + DOT_INSET * 2)
                    .height(DOT + DOT_INSET * 2 + DOT_TRAVEL)
                    .onGloballyPositioned { track = it.boundsInRoot() }
                    .border(2.dp, primary.copy(alpha = 0.6f), CircleShape),
                contentAlignment = Alignment.BottomCenter,
            ) {
                Box(
                    Modifier
                        .padding(bottom = DOT_INSET)
                        .offset(y = -DOT_TRAVEL * up)
                        .alpha(if (drag != null) 1f else 1f - rise * rise)
                        .size(DOT)
                        .background(tertiary, CircleShape),
                )
            }
            Spacer(Modifier.height(16.dp))
            Text(if (locked) "SWIPE UP TO UNLOCK" else "SWIPE UP", style = hudCaption().copy(color = onSurfaceVariant))
        }
    }

    /**
     * Slipstream's name; what's on the head unit, and its back and home, large, or between
     * sessions where things are up to; sleep, as large; the phone's own buttons; then ending it
     * all.
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
                        SleepButton()
                        PhoneButtons()
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
                        SleepButton()
                        PhoneButtons()
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

    /** The screen's other main action, so wide where the rest are round. */
    @Composable
    private fun SleepButton() {
        Box(
            Modifier
                .width(200.dp)
                .height(88.dp)
                .tappable(CircleShape) { sleep(true) }
                .background(primary),
            contentAlignment = Alignment.Center,
        ) {
            Icon(painterResource(R.drawable.ic_sl_dark), "Sleep", tint = onPrimary, modifier = Modifier.size(40.dp))
        }
    }

    /** Slipstream's settings and the phone's apps, over this as picture-in-picture. */
    @Composable
    private fun PhoneButtons() {
        Row(horizontalArrangement = Arrangement.spacedBy(20.dp)) {
            RoundButton(R.drawable.settings_24, "Slipstream settings", 56.dp) {
                shrink()
                startActivity(Intent(this@DashboardActivity, SettingsActivity::class.java))
            }
            RoundButton(R.drawable.ic_sl_phone, "Phone apps", 56.dp) {
                if (!shrink()) startActivity(Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_HOME))
            }
        }
    }

    /** Ends it all: a dot dragged the length of its track, from its start, which a pocket won't. */
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
                Text("SWIPE TO END SESSION", style = hudCaption().copy(color = onSurfaceVariant))
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

/** The swipe hint's dot, its inset in the track, and how far up the track it travels. */
private val DOT = 40.dp
private val DOT_INSET = 8.dp
private val DOT_TRAVEL = 104.dp
/** How far outside the track a swipe may start. */
private val TRACK_SLOP = 24.dp
/** The end swipe's track length, and how far along it the dot travels. */
private val END_TRACK = 280.dp
private val END_TRAVEL = END_TRACK - DOT - DOT_INSET * 2
