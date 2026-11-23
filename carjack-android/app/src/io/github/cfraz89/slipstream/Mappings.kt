package io.github.cfraz89.slipstream

import android.content.Context
import android.util.Log
import org.json.JSONArray
import org.json.JSONObject
import java.util.UUID

/** A switchgear input, as the core reports it: an Android key code. */
enum class Input(val code: Int, val title: String) {
    UP(19, "Up"),
    DOWN(20, "Down"),
    LEFT(21, "Left"),
    RIGHT(22, "Right"),
    SELECT(23, "Select"),
    BACK(4, "Back"),
    HOME(3, "Home"),
    VOICE(231, "Voice");

    companion object {
        fun of(code: Int) = entries.firstOrNull { it.code == code }
    }
}

/** What an input does, in Slipstream mode and Android Auto alike. */
enum class Action(val title: String) {
    NEXT("Next item"),
    PREVIOUS("Previous item"),
    UP("Up"),
    DOWN("Down"),
    LEFT("Left"),
    RIGHT("Right"),
    SELECT("Select"),
    BACK("Back"),
    HOME("Home"),
    VOICE("Voice assistant"),
    NONE("Nothing"),
}

/**
 * Inputs mapped to actions, and to others on a double or long press; see [Presses]. Media keys
 * aren't mapped; they always pass through.
 */
data class Mapping(
    val id: String,
    val name: String,
    val actions: Map<Input, Action>,
    val builtIn: Boolean,
    /** Who it's for, shown under its name. */
    val description: String = "",
    val doublePress: Map<Input, Action> = emptyMap(),
    val longPress: Map<Input, Action> = emptyMap(),
) {
    fun action(input: Input) = actions[input] ?: Action.NONE
}

/** The head unit as it last identified itself; [vehicle] the name of the vehicle it's in, where it says. */
data class HeadUnit(
    val bluetoothName: String?,
    val name: String?,
    val model: String?,
    val manufacturer: String?,
    val vehicle: String? = null,
)

/**
 * The switchgear mappings: built in per unit model, and custom ones. A profile gets its unit's
 * model's first built-in mapping unless another is chosen for it in settings.
 */
object Mappings {
    private const val TAG = "carjack"
    private const val PREF_CUSTOM = "profiles_custom"
    private const val PREF_CHOSEN = "mapping"
    private const val PREF_UNIT = "unit"

    /** Each input as its nearest Android key, for units without a mapping of their own. */
    private val direct = Mapping(
        "direct", "Direct",
        mapOf(
            Input.UP to Action.UP, Input.DOWN to Action.DOWN,
            Input.LEFT to Action.LEFT, Input.RIGHT to Action.RIGHT,
            Input.SELECT to Action.SELECT, Input.BACK to Action.BACK,
            Input.HOME to Action.HOME, Input.VOICE to Action.VOICE,
        ),
        true, "Unmapped: each input as its nearest Android key",
    )

    /** A built-in mapping and the units it's for. */
    private class BuiltIn(val mapping: Mapping, val matches: (HeadUnit) -> Boolean)

    /**
     * The 1000MT-X's dash, by the model it identifies as. Its d-pad reaches CarPlay as a dial for
     * up and down, select for right and back for left.
     */
    private fun cfmoto(unit: HeadUnit) = unit.model.equals("FA85", ignoreCase = true)

    /** A unit's first match is its automatic one. */
    private val builtIns = listOf(
        BuiltIn(
            Mapping(
                "cfmoto-1000mtx-dpad", "CFMoto 1000MT-X d-pad",
                mapOf(
                    Input.UP to Action.UP, Input.DOWN to Action.DOWN,
                    Input.BACK to Action.LEFT, Input.SELECT to Action.RIGHT,
                    Input.VOICE to Action.SELECT,
                ),
                true, "Arrows move; the middle button selects, double press for back, hold for home",
                doublePress = mapOf(Input.VOICE to Action.BACK),
                longPress = mapOf(Input.VOICE to Action.HOME),
            ),
            ::cfmoto,
        ),
        BuiltIn(
            Mapping(
                "cfmoto-1000mtx-standard", "CFMoto 1000MT-X standard",
                mapOf(
                    Input.UP to Action.PREVIOUS, Input.DOWN to Action.NEXT,
                    Input.BACK to Action.BACK, Input.SELECT to Action.SELECT,
                    Input.VOICE to Action.VOICE,
                ),
                true, "Up and down step through items, left is back, right selects",
            ),
            ::cfmoto,
        ),
    )

    @Volatile private var cached: Mapping? = null

    /** From the session, as the unit identifies itself: remembered, and its mapping picked. */
    fun identified(context: Context, unit: HeadUnit) {
        Log.i(TAG, "Head unit: $unit")
        Variant.prefs(context).edit().putString(
            PREF_UNIT,
            JSONObject().apply {
                put("bluetooth", unit.bluetoothName)
                put("name", unit.name)
                put("model", unit.model)
                put("manufacturer", unit.manufacturer)
                put("vehicle", unit.vehicle)
            }.toString(),
        ).apply()
        cached = null
    }

    /** The unit last connected, or null before any has. */
    fun unit(context: Context): HeadUnit? {
        val json = Variant.prefs(context).getString(PREF_UNIT, null) ?: return null
        return try {
            val o = JSONObject(json)
            HeadUnit(
                o.optStringOrNull("bluetooth"), o.optStringOrNull("name"), o.optStringOrNull("model"),
                o.optStringOrNull("manufacturer"), o.optStringOrNull("vehicle"),
            )
        } catch (e: Exception) {
            null
        }
    }

    /** The mapping in force: cheap, for every key. */
    fun active(context: Context): Mapping = cached ?: resolve(context).also { cached = it }

    /** The built-ins for the last unit's model, its automatic one first; none for an unknown unit. */
    fun forUnit(context: Context): List<Mapping> {
        val unit = unit(context) ?: return emptyList()
        return builtIns.filter { it.matches(unit) }.map { it.mapping }
    }

    /** What the unit gets without a choice: its model's default, else [direct]. */
    fun automatic(context: Context): Mapping = forUnit(context).firstOrNull() ?: direct

    private fun resolve(context: Context): Mapping {
        val id = chosen(context) ?: return automatic(context)
        return all(context).firstOrNull { it.id == id } ?: automatic(context)
    }

    /** Direct, the units' built-ins, then custom mappings. */
    fun all(context: Context): List<Mapping> = listOf(direct) + builtIns.map { it.mapping } + custom(context)

    /** The mapping chosen for the profile, by id; null for automatic. */
    fun chosen(context: Context): String? = Variant.prefs(context).getString(PREF_CHOSEN, null)

    fun choose(context: Context, id: String?) {
        Variant.prefs(context).edit().apply { if (id == null) remove(PREF_CHOSEN) else putString(PREF_CHOSEN, id) }.apply()
        cached = null
    }

    /** Another profile's settings now: its unit and choice. */
    fun invalidate() {
        cached = null
    }

    /** Shared by every profile. */
    fun custom(context: Context): List<Mapping> {
        val json = Profiles.global(context).getString(PREF_CUSTOM, null) ?: return emptyList()
        return try {
            val array = JSONArray(json)
            (0 until array.length()).map { i ->
                val o = array.getJSONObject(i)
                Mapping(
                    o.getString("id"), o.getString("name"), readActions(o.getJSONObject("actions")), false, "Custom",
                    doublePress = o.optJSONObject("double")?.let(::readActions).orEmpty(),
                    longPress = o.optJSONObject("long")?.let(::readActions).orEmpty(),
                )
            }
        } catch (e: Exception) {
            Log.w(TAG, "Custom mappings unreadable", e)
            emptyList()
        }
    }

    /** Adds or replaces [mapping] among the custom ones. */
    fun save(context: Context, mapping: Mapping) {
        writeCustom(context, custom(context).filter { it.id != mapping.id } + mapping.copy(builtIn = false))
    }

    fun delete(context: Context, id: String) {
        if (chosen(context) == id) choose(context, null)
        writeCustom(context, custom(context).filter { it.id != id })
    }

    /** A custom copy of [from], to edit. */
    fun copyOf(from: Mapping) =
        from.copy(id = UUID.randomUUID().toString(), name = "${from.name} (custom)", builtIn = false, description = "Custom")

    private fun writeCustom(context: Context, mappings: List<Mapping>) {
        val array = JSONArray()
        for (m in mappings) {
            array.put(
                JSONObject().apply {
                    put("id", m.id)
                    put("name", m.name)
                    put("actions", writeActions(m.actions))
                    put("double", writeActions(m.doublePress))
                    put("long", writeActions(m.longPress))
                },
            )
        }
        Profiles.global(context).edit().putString(PREF_CUSTOM, array.toString()).apply()
        cached = null
    }

    private fun readActions(map: JSONObject): Map<Input, Action> = Input.entries.mapNotNull { input ->
        map.optStringOrNull(input.name)?.let { a -> runCatching { input to Action.valueOf(a) }.getOrNull() }
    }.toMap()

    private fun writeActions(actions: Map<Input, Action>) =
        JSONObject().apply { actions.forEach { (input, action) -> put(input.name, action.name) } }

    private fun JSONObject.optStringOrNull(key: String): String? =
        if (isNull(key)) null else optString(key).takeIf { it.isNotEmpty() }
}
