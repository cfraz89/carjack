package io.github.cfraz89.slipstream

import android.content.Context
import android.location.Location
import android.os.SystemClock
import android.util.Log
import io.github.cfraz89.slipstream.launcher.Weather
import org.json.JSONObject
import java.net.HttpURLConnection
import java.net.URL
import java.net.URLEncoder
import kotlin.math.abs
import kotlin.math.cos
import kotlin.math.hypot
import kotlin.math.sin

/**
 * The road the phone is on, from OpenStreetMap through Overpass: the roads around a point are
 * fetched once, and the nearest picked from them locally until the phone nears their edge.
 * Overpass can take half a minute to answer, so the area reaches further ahead the faster it goes.
 */
object Roads {
    private const val TAG = "SlipstreamRoads"
    private const val OVERPASS = "https://overpass-api.de/api/interpreter"
    private const val AGENT = "Slipstream (github.com/cfraz89/carjack)"
    private const val MIN_RADIUS_M = 300.0
    private const val MAX_RADIUS_M = 1500.0
    /** How long the area should last at the current speed: longer than a slow answer takes. */
    private const val AHEAD_S = 60.0
    /** Refetched once this far out from the centre, as a fraction of the radius. */
    private const val REFETCH_AT = 0.6
    private const val RETRY_MS = 30_000L
    /** Further than this from every road: off-road, or a road OSM hasn't got. */
    private const val MAX_DISTANCE_M = 40.0
    /** A road at an angle to the way the phone's heading counts this much further away. */
    private const val CROSSING_PENALTY_M = 25.0
    private const val TIMEOUT_MS = 45_000

    /** A speed limit as signed: [value] in mph or km/h. */
    data class Limit(val value: Int, val mph: Boolean)

    data class Road(val name: String?, val ref: String?, val limit: Limit?)

    private class Way(val road: Road, val lats: DoubleArray, val lons: DoubleArray)

    @Volatile private var ways = emptyList<Way>()
    private var centre: Location? = null
    private var radius = MIN_RADIUS_M
    private var lastTry = 0L
    private var fetching = false

    /** The road at [location], or null; fetches on a thread of its own when the area runs out. */
    fun at(context: Context, location: Location): Road? {
        synchronized(this) {
            val stale = centre?.let { it.distanceTo(location) > radius * REFETCH_AT } ?: true
            if (stale && !fetching && SystemClock.elapsedRealtime() - lastTry >= RETRY_MS) {
                lastTry = SystemClock.elapsedRealtime()
                fetching = true
                Thread({ refetch(context, location) }, "slipstream-roads").start()
            }
        }
        return nearest(location)
    }

    /** Centred half a radius ahead, along the heading. */
    private fun refetch(context: Context, location: Location) {
        val r = (location.speed * AHEAD_S).coerceIn(MIN_RADIUS_M, MAX_RADIUS_M)
        val ahead = Location(location)
        if (location.hasBearing() && location.speed > 3) {
            val bearing = Math.toRadians(location.bearing.toDouble())
            val shift = r / 2
            ahead.latitude += shift * cos(bearing) / 111_320.0
            ahead.longitude += shift * sin(bearing) / (111_320.0 * cos(Math.toRadians(location.latitude)))
        }
        try {
            val fetched = fetch(context, ahead, r.toInt())
            synchronized(this) {
                ways = fetched
                centre = ahead
                radius = r
            }
        } catch (e: Exception) {
            Log.w(TAG, "No roads", e)
        } finally {
            synchronized(this) { fetching = false }
        }
    }

    private fun nearest(location: Location): Road? {
        val lat = location.latitude
        val lon = location.longitude
        // Metres per degree, near enough over a few hundred metres.
        val my = 111_320.0
        val mx = my * cos(Math.toRadians(lat))
        val heading = if (location.hasBearing() && location.speed > 3) location.bearing.toDouble() else null
        var best: Way? = null
        var bestScore = Double.MAX_VALUE
        for (way in ways) {
            for (i in 0 until way.lats.size - 1) {
                val ax = (way.lons[i] - lon) * mx
                val ay = (way.lats[i] - lat) * my
                val bx = (way.lons[i + 1] - lon) * mx
                val by = (way.lats[i + 1] - lat) * my
                var score = distanceToSegment(ax, ay, bx, by)
                if (score > MAX_DISTANCE_M) continue
                if (heading != null) {
                    val bearing = Math.toDegrees(Math.atan2(bx - ax, by - ay))
                    val off = abs(((bearing - heading) % 180 + 270) % 180 - 90)
                    if (off > 45) score += CROSSING_PENALTY_M
                }
                if (score < bestScore) {
                    bestScore = score
                    best = way
                }
            }
        }
        return best?.road
    }

    /** From the origin to the segment a-b. */
    private fun distanceToSegment(ax: Double, ay: Double, bx: Double, by: Double): Double {
        val dx = bx - ax
        val dy = by - ay
        val length = dx * dx + dy * dy
        val t = if (length == 0.0) 0.0 else (-(ax * dx + ay * dy) / length).coerceIn(0.0, 1.0)
        return hypot(ax + t * dx, ay + t * dy)
    }

    /** Roads a vehicle rides on: no service roads (driveways, car parks), paths or tracks. */
    private fun fetch(context: Context, location: Location, radius: Int): List<Way> {
        val network = Weather.internet(context) ?: throw IllegalStateException("No internet")
        val query = "[out:json][timeout:40];" +
            "way(around:$radius,${location.latitude},${location.longitude})" +
            "[highway~\"^(motorway|trunk|primary|secondary|tertiary|unclassified|residential|" +
            "living_street|road)(_link)?$\"];out tags geom;"
        val connection = network.openConnection(URL(OVERPASS)) as HttpURLConnection
        try {
            connection.connectTimeout = TIMEOUT_MS
            connection.readTimeout = TIMEOUT_MS
            connection.requestMethod = "POST"
            connection.doOutput = true
            connection.setRequestProperty("User-Agent", AGENT)
            connection.outputStream.use {
                it.write(("data=" + URLEncoder.encode(query, "UTF-8")).toByteArray())
            }
            val body = connection.inputStream.bufferedReader().use { it.readText() }
            val elements = JSONObject(body).getJSONArray("elements")
            return (0 until elements.length()).mapNotNull { i ->
                val element = elements.getJSONObject(i)
                val geometry = element.optJSONArray("geometry") ?: return@mapNotNull null
                val tags = element.optJSONObject("tags") ?: JSONObject()
                Way(
                    Road(
                        tags.optString("name").ifEmpty { null },
                        tags.optString("ref").ifEmpty { null },
                        limit(tags.optString("maxspeed")),
                    ),
                    DoubleArray(geometry.length()) { geometry.getJSONObject(it).getDouble("lat") },
                    DoubleArray(geometry.length()) { geometry.getJSONObject(it).getDouble("lon") },
                )
            }.also { Log.i(TAG, "${it.size} roads around the phone") }
        } finally {
            connection.disconnect()
        }
    }

    /** OSM's maxspeed: "60" (km/h) or "30 mph"; zone codes and "none" aren't numbers to show. */
    private fun limit(tag: String): Limit? {
        val match = Regex("""^(\d+)\s*(mph)?$""").find(tag.trim()) ?: return null
        return Limit(match.groupValues[1].toInt(), match.groupValues[2].isNotEmpty())
    }
}
