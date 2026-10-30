package io.github.cfraz89.slipstream

import android.Manifest
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
import android.location.Location
import android.location.LocationListener
import android.location.LocationManager
import android.os.BatteryManager
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import android.text.format.DateFormat
import android.util.Rational
import android.view.KeyEvent
import android.view.WindowInsets
import android.view.WindowInsetsController
import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.systemBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import io.github.cfraz89.slipstream.launcher.BatteryIcon
import io.github.cfraz89.slipstream.launcher.DrawableImage
import io.github.cfraz89.slipstream.launcher.Hud
import io.github.cfraz89.slipstream.launcher.LauncherTheme
import io.github.cfraz89.slipstream.launcher.Palette
import io.github.cfraz89.slipstream.launcher.Weather
import io.github.cfraz89.slipstream.launcher.hudCaption
import io.github.cfraz89.slipstream.launcher.hudText
import io.github.cfraz89.slipstream.launcher.tappable
import kotlinx.coroutines.delay
import java.lang.ref.WeakReference
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import java.util.concurrent.Executors
import kotlin.math.roundToInt

/**
 * The phone's screen during a session: what's on the head unit, with its back and home, and the
 * phone's heat or battery once they matter; or, as an option, the ride's figures - speed against
 * the road's limit, the road, the time, the weather and how the phone is faring. It keeps the screen on, so the phone neither sleeps (dropping the head unit's touches) nor
 * locks; covered (in a pocket), it goes black, dimmed right down, and takes no touches.
 */
class DashboardActivity : ComponentActivity() {
    companion object {
        private const val WEATHER_MS = 60_000L
        private const val HEAD_UNIT_MS = 2_000L
        /** Uncovered for this long before it lights again: a pocket's jostling isn't uncovering. */
        private const val UNCOVER_MS = 1_000L
        private val MPH_COUNTRIES = setOf("US", "GB", "LR", "MM")
        private const val MS_TO_KMH = 3.6
        private const val KMH_PER_MPH = 1.609344
        private const val ACTION_BACK = "io.github.cfraz89.slipstream.dashboard.BACK"
        private const val ACTION_HOME = "io.github.cfraz89.slipstream.dashboard.HOME"

        private var current = WeakReference<DashboardActivity>(null)

        fun close() {
            current.get()?.let { it.runOnUiThread { it.finish() } }
        }
    }

    private val mph = Locale.getDefault().country in MPH_COUNTRIES
    private val main = Handler(Looper.getMainLooper())
    private val background = Executors.newSingleThreadExecutor()

    private var covered by mutableStateOf(false)
    private var speed by mutableStateOf<Int?>(null)
    private var road by mutableStateOf<Roads.Road?>(null)
    private var weather by mutableStateOf<Weather.Now?>(null)
    private var battery by mutableStateOf<Battery?>(null)
    private var thermal by mutableStateOf(PowerManager.THERMAL_STATUS_NONE)
    private var headUnitPackage by mutableStateOf<String?>(null)
    /** The ride's figures as well as the controls ([Variant.dashboardRide]). */
    private var ride by mutableStateOf(false)
    private var pip by mutableStateOf(false)

    private class Battery(val percent: Int, val charging: Boolean, val celsius: Float)

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
        setContent { Dashboard() }
    }

    override fun onStart() {
        super.onStart()
        sensors()?.let { sm ->
            sm.getDefaultSensor(Sensor.TYPE_PROXIMITY)?.let {
                sm.registerListener(proximity, it, SensorManager.SENSOR_DELAY_NORMAL)
            }
        }
        registerReceiver(batteryChanged, IntentFilter(Intent.ACTION_BATTERY_CHANGED))
        getSystemService(PowerManager::class.java).let {
            thermal = it.currentThermalStatus
            it.addThermalStatusListener(mainExecutor, thermalChanged)
        }
        main.post(pollHeadUnit)
        ride = Variant.dashboardRide(this)
        if (ride) startRide()
    }

    override fun onStop() {
        sensors()?.unregisterListener(proximity)
        unregisterReceiver(batteryChanged)
        getSystemService(PowerManager::class.java).removeThermalStatusListener(thermalChanged)
        main.removeCallbacks(pollHeadUnit)
        main.removeCallbacks(uncover)
        stopRide()
        cover(false)
        super.onStop()
    }

    override fun onDestroy() {
        unregisterReceiver(pipAction)
        background.shutdown()
        super.onDestroy()
    }

    /** The ride layout's GPS and weather, which the controls don't need. */
    private fun startRide() {
        val fine = checkSelfPermission(Manifest.permission.ACCESS_FINE_LOCATION) == PackageManager.PERMISSION_GRANTED
        if (fine) {
            getSystemService(LocationManager::class.java)
                .requestLocationUpdates(LocationManager.GPS_PROVIDER, 1000L, 0f, location, Looper.getMainLooper())
        }
        main.post(pollWeather)
    }

    private fun stopRide() {
        getSystemService(LocationManager::class.java).removeUpdates(location)
        main.removeCallbacks(pollWeather)
    }

    /** Between the controls and the ride layout, kept for next time. */
    private fun flip() {
        stopRide()
        ride = !ride
        Variant.prefs(this).edit().putString("dashboard", if (ride) "ride" else "controls").apply()
        if (ride) startRide()
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
        if (inPip) cover(false)
    }

    /** Out of the way as a picture-in-picture window, the phone's own screen showing beneath. */
    private fun shrink(): Boolean = try {
        enterPictureInPictureMode(pipParams())
    } catch (e: IllegalStateException) {
        false
    }

    private fun sensors() = getSystemService(SensorManager::class.java)

    private val proximity = object : SensorEventListener {
        override fun onSensorChanged(event: SensorEvent) {
            val near = event.values[0] < event.sensor.maximumRange
            main.removeCallbacks(uncover)
            if (near) cover(true) else if (covered) main.postDelayed(uncover, UNCOVER_MS)
        }

        override fun onAccuracyChanged(sensor: Sensor, accuracy: Int) {}
    }

    private val uncover = Runnable { cover(false) }

    /** Dimmed right down, and the system bars away so a pocket can't swipe them open. */
    private fun cover(value: Boolean) {
        if (covered == value || (value && pip)) return
        covered = value
        window.attributes = window.attributes.apply {
            screenBrightness = if (value) 0f else WindowManager.LayoutParams.BRIGHTNESS_OVERRIDE_NONE
        }
        window.insetsController?.let {
            if (value) {
                it.systemBarsBehavior = WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
                it.hide(WindowInsets.Type.systemBars())
            } else {
                it.show(WindowInsets.Type.systemBars())
            }
        }
    }

    private val location = LocationListener { fix: Location ->
        speed = if (fix.hasSpeed()) {
            val kmh = fix.speed * MS_TO_KMH
            (if (mph) kmh / KMH_PER_MPH else kmh).roundToInt()
        } else {
            null
        }
        road = Roads.at(this, fix)
    }

    private val batteryChanged = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            val level = intent.getIntExtra(BatteryManager.EXTRA_LEVEL, -1)
            val scale = intent.getIntExtra(BatteryManager.EXTRA_SCALE, 100)
            if (level < 0) return
            battery = Battery(
                level * 100 / scale,
                intent.getIntExtra(BatteryManager.EXTRA_PLUGGED, 0) != 0,
                intent.getIntExtra(BatteryManager.EXTRA_TEMPERATURE, 0) / 10f,
            )
        }
    }

    private val thermalChanged = PowerManager.OnThermalStatusChangedListener { thermal = it }

    private val pollWeather = object : Runnable {
        override fun run() {
            background.execute {
                val now = Weather.now(this@DashboardActivity)
                main.post { weather = now }
            }
            main.postDelayed(this, WEATHER_MS)
        }
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
            if (covered) {
                // Every touch taken here, and none acted on.
                Box(Modifier.fillMaxSize().pointerInput(Unit) {
                    awaitPointerEventScope { while (true) awaitPointerEvent().changes.forEach { it.consume() } }
                })
                return@Box
            }
            when {
                pip -> Box(Modifier.fillMaxSize().padding(12.dp), contentAlignment = Alignment.Center) {
                    val pkg = headUnitPackage
                    DrawableImage(remember(pkg) { pkg?.let(::appIcon) }, Modifier.fillMaxSize(0.7f))
                }
                ride -> Ride()
                else -> Controls()
            }
        }
    }

    /** What's on the head unit, its back and home, and only what needs attention. */
    @Composable
    private fun Controls() {
        BoxWithConstraints(Modifier.fillMaxSize().systemBarsPadding().padding(24.dp)) {
            if (maxWidth > maxHeight) {
                Row(Modifier.fillMaxSize(), verticalAlignment = Alignment.CenterVertically) {
                    Column(
                        Modifier.weight(1f),
                        horizontalAlignment = Alignment.CenterHorizontally,
                        verticalArrangement = Arrangement.spacedBy(16.dp),
                    ) {
                        OnHeadUnit()
                        Alerts()
                    }
                    Column(
                        Modifier.weight(1f),
                        horizontalAlignment = Alignment.CenterHorizontally,
                        verticalArrangement = Arrangement.spacedBy(32.dp),
                    ) {
                        HeadUnitButtons(88.dp)
                        PhoneButtons()
                    }
                }
            } else {
                Column(
                    Modifier.fillMaxSize(),
                    horizontalAlignment = Alignment.CenterHorizontally,
                    verticalArrangement = Arrangement.SpaceBetween,
                ) {
                    Alerts()
                    OnHeadUnit()
                    HeadUnitButtons(88.dp)
                    PhoneButtons()
                }
            }
        }
    }

    @Composable
    private fun OnHeadUnit() {
        val pkg = headUnitPackage
        val icon = remember(pkg) { pkg?.let(::appIcon) }
        val label = remember(pkg) { pkg?.let(::appLabel) }
        Column(horizontalAlignment = Alignment.CenterHorizontally) {
            DrawableImage(icon, Modifier.size(112.dp))
            Spacer(Modifier.height(16.dp))
            Text(label.orEmpty(), style = hudText(34.sp, FontWeight.SemiBold), maxLines = 1)
            Text("ON HEAD UNIT", style = hudCaption())
        }
    }

    /** The phone's heat and a low battery, only once they matter. */
    @Composable
    private fun Alerts() {
        Column(horizontalAlignment = Alignment.CenterHorizontally) {
            val (heat, hot) = heat()
            if (thermal >= PowerManager.THERMAL_STATUS_LIGHT) {
                Text(heat, style = hudText(26.sp, FontWeight.SemiBold, if (hot) WARN else Palette.text))
            }
            battery?.let { b ->
                if (b.percent <= 15 && !b.charging) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        BatteryIcon(b.percent, false, Modifier.size(26.dp))
                        Spacer(Modifier.width(6.dp))
                        Text("${b.percent}%", style = hudText(26.sp, FontWeight.SemiBold, WARN))
                    }
                }
            }
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
        Row(horizontalArrangement = Arrangement.spacedBy(20.dp)) {
            RoundButton(R.drawable.ic_sl_arrow_back, "Back", size) {
                background.execute { Variant.key(this@DashboardActivity, KeyEvent.KEYCODE_BACK) }
            }
            RoundButton(R.drawable.ic_sl_home, "Home", size) {
                background.execute { Variant.goHome(this@DashboardActivity) }
            }
            if (!Variant.opensLauncher(this@DashboardActivity)) {
                RoundButton(R.drawable.ic_apps, "Launcher", size) {
                    background.execute { Variant.openLauncher() }
                }
            }
        }
    }

    /** The other layout; Slipstream's settings and the phone's apps, over this as picture-in-picture. */
    @Composable
    private fun PhoneButtons() {
        Row(horizontalArrangement = Arrangement.spacedBy(16.dp)) {
            RoundButton(
                if (ride) R.drawable.ic_sl_dpad else R.drawable.ic_sl_speed,
                if (ride) "Controls" else "Ride info",
                56.dp,
            ) { flip() }
            RoundButton(R.drawable.settings_24, "Slipstream settings", 56.dp) {
                shrink()
                startActivity(Intent(this@DashboardActivity, MainActivity::class.java))
            }
            RoundButton(R.drawable.ic_sl_phone, "Phone apps", 56.dp) {
                if (!shrink()) startActivity(Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_HOME))
            }
        }
    }

    /** The ride's figures: speed against the limit, the road, the time, weather and the phone. */
    @Composable
    private fun Ride() {
        BoxWithConstraints(Modifier.fillMaxSize().systemBarsPadding().padding(24.dp)) {
            if (maxWidth > maxHeight) {
                Row(Modifier.fillMaxSize(), verticalAlignment = Alignment.CenterVertically) {
                    Column(Modifier.weight(1f)) { Speed() }
                    Spacer(Modifier.width(24.dp))
                    Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(20.dp)) {
                        Clock()
                        Status()
                        Buttons()
                    }
                }
            } else {
                Column(Modifier.fillMaxSize(), verticalArrangement = Arrangement.SpaceBetween) {
                    Clock()
                    Speed()
                    Column(verticalArrangement = Arrangement.spacedBy(20.dp)) {
                        Status()
                        Buttons()
                    }
                }
            }
        }
    }

    @Composable
    private fun Clock() {
        var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
        LaunchedEffect(Unit) {
            while (true) {
                delay(60_000 - System.currentTimeMillis() % 60_000)
                now = System.currentTimeMillis()
            }
        }
        val locale = Locale.getDefault()
        val pattern = DateFormat.getBestDateTimePattern(locale, if (DateFormat.is24HourFormat(this)) "Hm" else "hm")
        Column {
            Text(SimpleDateFormat(pattern, locale).format(Date(now)), style = hudText(64.sp, FontWeight.SemiBold))
            Text(
                SimpleDateFormat(DateFormat.getBestDateTimePattern(locale, "EEEEdMMMM"), locale)
                    .format(Date(now)).uppercase(locale),
                style = hudCaption(),
                maxLines = 1,
            )
        }
    }

    /** The speed, the limit beside it as signed, and the road beneath. */
    @Composable
    private fun Speed() {
        val limit = road?.limit?.let { shown(it) }
        val over = limit != null && speed != null && speed!! > limit
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f, fill = false)) {
                Text(
                    speed?.toString() ?: "–",
                    style = hudText(144.sp, FontWeight.SemiBold, if (over) WARN else Palette.text),
                    maxLines = 1,
                )
                Text(if (mph) "MPH" else "KM/H", style = hudCaption())
            }
            if (limit != null) {
                Spacer(Modifier.width(28.dp))
                Box(
                    Modifier
                        .size(116.dp)
                        .background(Color.White, CircleShape)
                        .border(12.dp, LIMIT_RING, CircleShape),
                    contentAlignment = Alignment.Center,
                ) {
                    Text(
                        limit.toString(),
                        style = TextStyle(fontFamily = Hud, fontWeight = FontWeight.SemiBold, fontSize = 48.sp),
                        color = Color.Black,
                    )
                }
            }
        }
        val r = road
        val name = listOfNotNull(r?.ref, r?.name).joinToString("  ·  ")
        if (name.isNotEmpty()) {
            Spacer(Modifier.height(12.dp))
            Text(name, style = hudText(30.sp), maxLines = 2, overflow = TextOverflow.Ellipsis)
        }
    }

    /** In the units the speed's in. */
    private fun shown(limit: Roads.Limit): Int = when {
        limit.mph == mph -> limit.value
        mph -> (limit.value / KMH_PER_MPH).roundToInt()
        else -> (limit.value * KMH_PER_MPH).roundToInt()
    }

    /** Weather, battery and the phone's heat. */
    @Composable
    private fun Status() {
        val locale = Locale.getDefault()
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
            weather?.let { w ->
                Column {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Icon(painterResource(w.icon), null, tint = Palette.accent, modifier = Modifier.size(30.dp))
                        Spacer(Modifier.width(8.dp))
                        Text("${w.temperature}°", style = hudText(32.sp, FontWeight.SemiBold))
                    }
                    Text(w.sky.uppercase(locale), style = hudCaption(), maxLines = 1)
                }
            }
            battery?.let { b ->
                Column(horizontalAlignment = Alignment.CenterHorizontally) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        BatteryIcon(b.percent, b.charging, Modifier.size(30.dp))
                        Spacer(Modifier.width(6.dp))
                        Text(
                            "${b.percent}%",
                            style = hudText(
                                32.sp, FontWeight.SemiBold,
                                if (b.percent <= 15 && !b.charging) WARN else Palette.text,
                            ),
                        )
                    }
                    Text(if (b.charging) "CHARGING" else "BATTERY", style = hudCaption())
                }
                val (heat, hot) = heat()
                Column(horizontalAlignment = Alignment.End) {
                    Text(
                        "${b.celsius.roundToInt()}°",
                        style = hudText(32.sp, FontWeight.SemiBold, if (hot) WARN else Palette.text),
                    )
                    Text(heat, style = hudCaption())
                }
            }
        }
    }

    /** The phone's own read of its heat, and whether it's slowing down for it. */
    private fun heat(): Pair<String, Boolean> = when (thermal) {
        PowerManager.THERMAL_STATUS_NONE -> "PHONE" to false
        PowerManager.THERMAL_STATUS_LIGHT -> "PHONE WARM" to false
        PowerManager.THERMAL_STATUS_MODERATE -> "PHONE HOT" to true
        else -> "PHONE TOO HOT" to true
    }

    /** The head unit's back and home; Slipstream's settings and the phone's own apps. */
    @Composable
    private fun Buttons() {
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
            HeadUnitButtons(56.dp)
            PhoneButtons()
        }
    }

    @Composable
    private fun RoundButton(icon: Int, description: String, size: Dp, onClick: () -> Unit) {
        Box(
            Modifier.size(size).tappable(CircleShape, onClick = onClick).background(Palette.tonal),
            contentAlignment = Alignment.Center,
        ) {
            Icon(painterResource(icon), description, tint = Palette.onTonal, modifier = Modifier.size(size * 0.44f))
        }
    }
}

private val WARN = Color(0xFFFFB4AB)
private val LIMIT_RING = Color(0xFFD32F2F)
