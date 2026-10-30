package io.github.cfraz89.slipstream

import android.Manifest
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
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
import androidx.compose.material3.TextButton
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
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import io.github.cfraz89.slipstream.launcher.Weather
import kotlinx.coroutines.delay
import java.lang.ref.WeakReference
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import java.util.concurrent.Executors
import kotlin.math.roundToInt

/**
 * The phone's screen during a session: what the head unit doesn't show, readable at a glance -
 * speed against the road's limit, the road, the time, the weather and how the phone is faring.
 * It keeps the screen on, so the phone neither sleeps (dropping the head unit's touches) nor
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
    private var headUnitApp by mutableStateOf<String?>(null)

    private class Battery(val percent: Int, val charging: Boolean, val celsius: Float)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        current = WeakReference(this)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        window.setDecorFitsSystemWindows(false)
        setContent { Dashboard() }
    }

    override fun onStart() {
        super.onStart()
        sensors()?.let { sm ->
            sm.getDefaultSensor(Sensor.TYPE_PROXIMITY)?.let {
                sm.registerListener(proximity, it, SensorManager.SENSOR_DELAY_NORMAL)
            }
        }
        if (checkSelfPermission(Manifest.permission.ACCESS_FINE_LOCATION) == PackageManager.PERMISSION_GRANTED) {
            getSystemService(LocationManager::class.java)
                .requestLocationUpdates(LocationManager.GPS_PROVIDER, 1000L, 0f, location, Looper.getMainLooper())
        }
        registerReceiver(batteryChanged, IntentFilter(Intent.ACTION_BATTERY_CHANGED))
        getSystemService(PowerManager::class.java).let {
            thermal = it.currentThermalStatus
            it.addThermalStatusListener(mainExecutor, thermalChanged)
        }
        main.post(pollWeather)
        main.post(pollHeadUnit)
    }

    override fun onStop() {
        sensors()?.unregisterListener(proximity)
        getSystemService(LocationManager::class.java).removeUpdates(location)
        unregisterReceiver(batteryChanged)
        getSystemService(PowerManager::class.java).removeThermalStatusListener(thermalChanged)
        main.removeCallbacks(pollWeather)
        main.removeCallbacks(pollHeadUnit)
        main.removeCallbacks(uncover)
        cover(false)
        super.onStop()
    }

    override fun onDestroy() {
        background.shutdown()
        super.onDestroy()
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
        if (covered == value) return
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
                val label = ShizukuDisplay.topPackage()?.let(::appLabel)
                main.post { headUnitApp = label }
            }
            main.postDelayed(this, HEAD_UNIT_MS)
        }
    }

    private fun appLabel(pkg: String): String = when (pkg) {
        packageName -> "Slipstream launcher"
        else -> try {
            packageManager.getApplicationLabel(packageManager.getApplicationInfo(pkg, 0)).toString()
        } catch (e: PackageManager.NameNotFoundException) {
            pkg
        }
    }

    @Composable
    private fun Dashboard() {
        Box(Modifier.fillMaxSize().background(Color.Black)) {
            if (covered) {
                // Every touch taken here, and none acted on.
                Box(Modifier.fillMaxSize().pointerInput(Unit) {
                    awaitPointerEventScope { while (true) awaitPointerEvent().changes.forEach { it.consume() } }
                })
                return@Box
            }
            BoxWithConstraints(Modifier.fillMaxSize().systemBarsPadding().padding(24.dp)) {
                if (maxWidth > maxHeight) {
                    Row(Modifier.fillMaxSize(), verticalAlignment = Alignment.CenterVertically) {
                        Column(Modifier.weight(1f)) { Speed() }
                        Spacer(Modifier.width(24.dp))
                        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(20.dp)) {
                            Clock()
                            Status()
                            HeadUnit()
                        }
                    }
                } else {
                    Column(Modifier.fillMaxSize(), verticalArrangement = Arrangement.SpaceBetween) {
                        Clock()
                        Speed()
                        Column(verticalArrangement = Arrangement.spacedBy(20.dp)) {
                            Status()
                            HeadUnit()
                        }
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
        Text(SimpleDateFormat(pattern, locale).format(Date(now)), color = TEXT, fontSize = 56.sp)
    }

    /** The speed, the limit beside it as signed, and the road beneath. */
    @Composable
    private fun Speed() {
        val limit = road?.limit?.let { shown(it) }
        val over = limit != null && speed != null && speed!! > limit
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f, fill = false)) {
                Text(
                    speed?.toString() ?: "-",
                    color = if (over) WARN else TEXT,
                    fontSize = 120.sp,
                    fontWeight = FontWeight.Medium,
                )
                Text(if (mph) "mph" else "km/h", color = DIM, fontSize = 22.sp)
            }
            if (limit != null) {
                Spacer(Modifier.width(24.dp))
                Box(
                    Modifier
                        .size(112.dp)
                        .background(Color.White, CircleShape)
                        .border(12.dp, LIMIT_RING, CircleShape),
                    contentAlignment = Alignment.Center,
                ) {
                    Text(limit.toString(), color = Color.Black, fontSize = 44.sp, fontWeight = FontWeight.Bold)
                }
            }
        }
        val r = road
        val name = listOfNotNull(r?.ref, r?.name).joinToString(" · ")
        if (name.isNotEmpty()) {
            Spacer(Modifier.height(12.dp))
            Text(name, color = TEXT, fontSize = 28.sp, maxLines = 2)
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
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
            weather?.let { w ->
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Icon(painterResource(w.icon), null, tint = TEXT, modifier = Modifier.size(32.dp))
                    Spacer(Modifier.width(8.dp))
                    Text("${w.temperature}${w.unit}", color = TEXT, fontSize = 26.sp)
                }
            }
            battery?.let { b ->
                Text(
                    "${b.percent}%" + if (b.charging) " ⚡" else "",
                    color = if (b.percent <= 15 && !b.charging) WARN else TEXT,
                    fontSize = 26.sp,
                )
            }
            val (heat, hot) = heat()
            battery?.let { b ->
                Text("${b.celsius.roundToInt()}°C $heat", color = if (hot) WARN else TEXT, fontSize = 26.sp)
            }
        }
    }

    /** The phone's own read of its heat, and whether it's slowing down for it. */
    private fun heat(): Pair<String, Boolean> = when (thermal) {
        PowerManager.THERMAL_STATUS_NONE -> "" to false
        PowerManager.THERMAL_STATUS_LIGHT -> "warm" to false
        PowerManager.THERMAL_STATUS_MODERATE -> "hot" to true
        else -> "too hot" to true
    }

    @Composable
    private fun HeadUnit() {
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Text(
                headUnitApp?.let { "Head unit: $it" } ?: "",
                color = DIM,
                fontSize = 20.sp,
                modifier = Modifier.weight(1f),
            )
            TextButton(onClick = {
                startActivity(Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_HOME))
            }) { Text("Phone apps", color = ACCENT, fontSize = 18.sp) }
        }
    }
}

private val TEXT = Color(0xFFE2E2E9)
private val DIM = Color(0xFF8D9199)
private val WARN = Color(0xFFFFB4AB)
private val ACCENT = Color(0xFFA5C8FF)
private val LIMIT_RING = Color(0xFFD32F2F)
