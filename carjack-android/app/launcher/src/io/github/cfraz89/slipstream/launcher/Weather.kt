package io.github.cfraz89.slipstream.launcher

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.location.Location
import android.location.LocationManager
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.os.SystemClock
import android.util.Log
import org.json.JSONObject
import java.net.HttpURLConnection
import java.net.URL
import java.util.Locale
import kotlin.math.roundToInt

/**
 * The weather where the phone is, from Open-Meteo (free, no key): the air temperature the head
 * unit doesn't show, and a word for the sky. Approximate location only, and not at all without the
 * permission.
 */
object Weather {
    private const val TAG = "SlipstreamWeather"
    private const val FRESH_MS = 15 * 60 * 1000L
    private const val LOCATION_STALE_MS = 2 * 60 * 60 * 1000L
    private const val TIMEOUT_MS = 10_000

    data class Now(val temperature: Int, val unit: String, val sky: String, val icon: Int)

    private var cached: Now? = null
    private var fetchedAt = 0L

    /** The current weather, fetched at most every quarter hour. Blocks: off the main thread. */
    fun now(context: Context): Now? {
        val fresh = SystemClock.elapsedRealtime() - fetchedAt < FRESH_MS
        if (cached != null && fresh) return cached
        val location = location(context) ?: return cached
        val network = internet(context) ?: return cached
        return try {
            fetch(network, location).also {
                cached = it
                fetchedAt = SystemClock.elapsedRealtime()
            }
        } catch (e: Exception) {
            Log.w(TAG, "No weather", e)
            cached
        }
    }

    /** The freshest last-known fix any provider has, roughly where the phone is. */
    private fun location(context: Context): Location? {
        if (context.checkSelfPermission(Manifest.permission.ACCESS_COARSE_LOCATION)
            != PackageManager.PERMISSION_GRANTED
        ) return null
        val manager = context.getSystemService(LocationManager::class.java)
        return try {
            listOf(
                LocationManager.FUSED_PROVIDER,
                LocationManager.NETWORK_PROVIDER,
                LocationManager.GPS_PROVIDER,
            ).mapNotNull { manager.getLastKnownLocation(it) }
                .filter { System.currentTimeMillis() - it.time < LOCATION_STALE_MS }
                .maxByOrNull { it.time }
        } catch (e: SecurityException) {
            null
        }
    }

    /**
     * A network with internet. Not the process default: during a session the app is bound to the
     * head unit's WiFi, which has none.
     */
    fun internet(context: Context): Network? {
        val connectivity = context.getSystemService(ConnectivityManager::class.java)
        @Suppress("DEPRECATION")
        return connectivity.allNetworks.firstOrNull { network ->
            connectivity.getNetworkCapabilities(network)?.run {
                hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET) &&
                    hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED)
            } == true
        }
    }

    private fun fetch(network: Network, location: Location): Now {
        val fahrenheit = Locale.getDefault().country in setOf("US", "LR", "MM")
        val url = URL(
            "https://api.open-meteo.com/v1/forecast" +
                "?latitude=%.2f&longitude=%.2f".format(Locale.ROOT, location.latitude, location.longitude) +
                "&current=temperature_2m,weather_code" +
                if (fahrenheit) "&temperature_unit=fahrenheit" else "",
        )
        val connection = network.openConnection(url) as HttpURLConnection
        try {
            connection.connectTimeout = TIMEOUT_MS
            connection.readTimeout = TIMEOUT_MS
            val body = connection.inputStream.bufferedReader().use { it.readText() }
            val current = JSONObject(body).getJSONObject("current")
            val code = current.getInt("weather_code")
            return Now(
                temperature = current.getDouble("temperature_2m").roundToInt(),
                unit = if (fahrenheit) "°F" else "°C",
                sky = sky(code),
                icon = icon(code),
            )
        } finally {
            connection.disconnect()
        }
    }

    private fun icon(code: Int): Int = when (code) {
        0, 1 -> R.drawable.ic_weather_sun
        2 -> R.drawable.ic_weather_partly
        45, 48 -> R.drawable.ic_weather_fog
        in 51..67, in 80..82 -> R.drawable.ic_weather_rain
        in 71..77, 85, 86 -> R.drawable.ic_weather_snow
        in 95..99 -> R.drawable.ic_weather_storm
        else -> R.drawable.ic_weather_cloud
    }

    /** WMO weather interpretation codes, as Open-Meteo gives them. */
    private fun sky(code: Int): String = when (code) {
        0 -> "Clear"
        1 -> "Mostly clear"
        2 -> "Partly cloudy"
        3 -> "Overcast"
        45, 48 -> "Fog"
        in 51..57 -> "Drizzle"
        in 61..67 -> "Rain"
        in 71..77 -> "Snow"
        in 80..82 -> "Showers"
        85, 86 -> "Snow showers"
        in 95..99 -> "Thunderstorm"
        else -> ""
    }
}
