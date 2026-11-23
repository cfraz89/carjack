package io.github.cfraz89.slipstream

import android.Manifest
import android.bluetooth.BluetoothManager
import android.content.Context
import android.content.SharedPreferences
import android.content.pm.PackageManager
import android.util.Log
import io.github.cfraz89.slipstream.ui.R
import org.json.JSONArray
import org.json.JSONObject
import java.util.UUID

/** What a profile's shown with. */
enum class ProfileIcon(val res: Int, val title: String) {
    SCREEN(R.drawable.ic_sl_screen, "Screen"),
    CAR(R.drawable.ic_sl_car, "Car"),
    BIKE(R.drawable.ic_sl_bike, "Bike"),
}

/** A set of settings, under a name. */
data class Profile(val id: String, val name: String, val icon: ProfileIcon = ProfileIcon.SCREEN)

/**
 * The profiles, each with settings of its own, one of them selected: what [Variant.prefs] reads.
 * What isn't a profile's - the session's status, the phone's own Bluetooth address, the custom
 * switchgear mappings - stays in [global]. There's always one, from install on.
 */
object Profiles {
    private const val TAG = "carjack"
    private const val GLOBAL = Variant.PREFS
    // Named for vehicles, as they first were; kept so settings already stored still read.
    private const val KEY_PROFILES = "vehicles"
    private const val KEY_SELECTED = "vehicle"
    /**
     * The device a profile's sessions are with: an address or a name; blank to take the first to
     * connect ([AUTO_SET]), or [ANY] for whichever connects, each time.
     */
    const val TARGET = "target"
    const val AUTO_SET = ""
    const val ANY = "any"

    /** Keys in [GLOBAL] that stay there; any other moves to the first profile when profiles arrive. */
    private val GLOBAL_KEYS = setOf(
        KEY_PROFILES, KEY_SELECTED, "status", "retryable", "linked", "bluetooth_address",
        "aa_server_found", "crash_reports", "profiles_custom", "listening",
    )

    fun global(context: Context): SharedPreferences =
        context.applicationContext.getSharedPreferences(GLOBAL, Context.MODE_PRIVATE)

    /** The selected profile's settings. */
    fun prefs(context: Context): SharedPreferences = prefsOf(context, selected(context).id)

    private fun prefsOf(context: Context, id: String): SharedPreferences =
        context.applicationContext.getSharedPreferences("${GLOBAL}_vehicle_$id", Context.MODE_PRIVATE)

    @Synchronized
    fun all(context: Context): List<Profile> {
        val json = global(context).getString(KEY_PROFILES, null) ?: return listOf(firstProfile(context))
        return try {
            val array = JSONArray(json)
            (0 until array.length()).map {
                val o = array.getJSONObject(it)
                Profile(
                    o.getString("id"),
                    if (o.isNull("name")) defaultName(it) else o.getString("name"),
                    ProfileIcon.entries.firstOrNull { icon -> icon.name == o.optString("icon") } ?: ProfileIcon.SCREEN,
                )
            }.ifEmpty { listOf(firstProfile(context)) }
        } catch (e: Exception) {
            Log.w(TAG, "Profiles unreadable", e)
            listOf(firstProfile(context))
        }
    }

    /** "Profile 2" for the second, say: what a new one's offered as. */
    fun defaultName(index: Int) = "Profile ${index + 1}"

    fun selected(context: Context): Profile {
        val all = all(context)
        val id = global(context).getString(KEY_SELECTED, null)
        return all.firstOrNull { it.id == id } ?: all.first()
    }

    fun select(context: Context, id: String) {
        if (id == selected(context).id) return
        global(context).edit().putString(KEY_SELECTED, id).apply()
        Log.i(TAG, "Profile: ${all(context).firstOrNull { it.id == id }?.name}")
        Mappings.invalidate()
    }

    /** A new profile with default settings; selected. */
    fun add(context: Context, name: String, icon: ProfileIcon): Profile {
        val all = all(context)
        val profile = Profile(UUID.randomUUID().toString(), name.trim().ifEmpty { defaultName(all.size) }, icon)
        write(context, all + profile)
        select(context, profile.id)
        return profile
    }

    fun update(context: Context, id: String, name: String, icon: ProfileIcon) {
        write(context, all(context).map { if (it.id == id) it.copy(name = name.trim().ifEmpty { it.name }, icon = icon) else it })
    }

    /** All but the last one. */
    fun remove(context: Context, ids: Set<String>) {
        val rest = all(context).filter { it.id !in ids }
        if (rest.isEmpty()) return
        val wasSelected = selected(context).id in ids
        write(context, rest)
        for (id in ids) prefsOf(context, id).edit().clear().apply()
        if (wasSelected) {
            global(context).edit().putString(KEY_SELECTED, rest.first().id).apply()
            Mappings.invalidate()
        }
    }

    /** [id]'s device setting: an address or name, [AUTO_SET] or [ANY]. */
    fun targetSetting(context: Context, id: String): String =
        prefsOf(context, id).getString(TARGET, null)?.trim()?.let { if (it.equals("auto", ignoreCase = true)) AUTO_SET else it }
            ?: AUTO_SET

    /** [id]'s device, or null where it takes whichever connects. */
    fun target(context: Context, id: String): String? = targetSetting(context, id).takeIf(::isSpecific)

    /** Each profile's device, where it has one. */
    fun targets(context: Context): List<String> = all(context).mapNotNull { target(context, it.id) }

    private fun isSpecific(target: String) = target != AUTO_SET && target != ANY

    /**
     * From a session, as the device connects: the profile it belongs to is selected, and a selected
     * profile set to [AUTO_SET] takes this one.
     */
    fun connected(context: Context, address: String, bluetoothName: String?) {
        all(context).firstOrNull { p ->
            val target = target(context, p.id) ?: return@firstOrNull false
            target.equals(address, ignoreCase = true) || target == bluetoothName
        }?.let { select(context, it.id) }
        val profile = selected(context)
        if (targetSetting(context, profile.id) == AUTO_SET) {
            prefsOf(context, profile.id).edit().putString(TARGET, address).apply()
        }
    }

    /**
     * A profile set to [AUTO_SET] that has connected - set up before connecting set its device -
     * set to the paired device of the name it last connected with.
     */
    fun settle(context: Context) {
        if (context.checkSelfPermission(Manifest.permission.BLUETOOTH_CONNECT) != PackageManager.PERMISSION_GRANTED) return
        val adapter = context.getSystemService(BluetoothManager::class.java)?.adapter ?: return
        if (!adapter.isEnabled) return
        for (profile in all(context)) {
            if (targetSetting(context, profile.id) != AUTO_SET) continue
            val prefs = prefsOf(context, profile.id)
            val unitName = try {
                prefs.getString("unit", null)?.let { JSONObject(it).optString("bluetooth").takeIf(String::isNotEmpty) }
            } catch (e: Exception) {
                null
            } ?: continue
            val device = adapter.bondedDevices.firstOrNull { it.name == unitName } ?: continue
            prefs.edit().putString(TARGET, device.address).apply()
            Log.i(TAG, "Profile ${profile.name} set to $unitName")
        }
    }

    private fun write(context: Context, profiles: List<Profile>) {
        val array = JSONArray()
        for (p in profiles) array.put(json(p))
        global(context).edit().putString(KEY_PROFILES, array.toString()).apply()
    }

    private fun json(p: Profile) = JSONObject().put("id", p.id).put("name", p.name).put("icon", p.icon.name)

    /** The profile there is before any's added, with the settings from before profiles moved into it. */
    @Synchronized
    private fun firstProfile(context: Context): Profile {
        val global = global(context)
        val profile = Profile(UUID.randomUUID().toString(), defaultName(0))
        val moving = global.all.filterKeys { it !in GLOBAL_KEYS }
        prefsOf(context, profile.id).edit().apply {
            for ((key, value) in moving) {
                when (value) {
                    is String -> putString(key, value)
                    is Boolean -> putBoolean(key, value)
                    is Int -> putInt(key, value)
                    is Long -> putLong(key, value)
                    is Float -> putFloat(key, value)
                    is Set<*> -> putStringSet(key, value.filterIsInstance<String>().toSet())
                }
            }
        }.commit()
        global.edit().apply {
            moving.keys.forEach(::remove)
            putString(KEY_PROFILES, JSONArray().put(json(profile)).toString())
            putString(KEY_SELECTED, profile.id)
        }.commit()
        return profile
    }
}
