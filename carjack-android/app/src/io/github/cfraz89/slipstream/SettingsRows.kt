package io.github.cfraz89.slipstream

import android.Manifest
import android.bluetooth.BluetoothManager
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.platform.LocalContext
import io.github.cfraz89.slipstream.ui.R

// The settings both builds' screens have: each a row showing its current value, opening its
// choices. [onChange] tells the screen to read its settings afresh.

/** Display scales offered, by name, and the densities they set; 0 is automatic. */
private val SIZES = listOf("Default", "Smaller", "Larger", "Largest")
private val DENSITIES = intArrayOf(0, 160, 240, 280)

/** "Any", then each paired head unit, as name and address. */
fun pairedUnits(context: Context): List<Pair<String, String>> {
    val units = mutableListOf("Any" to "")
    val adapter = context.getSystemService(BluetoothManager::class.java).adapter
    if (adapter?.isEnabled == true &&
        context.checkSelfPermission(Manifest.permission.BLUETOOTH_CONNECT) == PackageManager.PERMISSION_GRANTED
    ) {
        for (device in adapter.bondedDevices) {
            if (Bluetooth.hostsIap(device.uuids)) units += (device.name ?: device.address) to device.address
        }
    }
    return units
}

@Composable
fun HeadUnitRow(onChange: () -> Unit) {
    val context = LocalContext.current
    val units = remember { pairedUnits(context) }
    val target = Variant.setting(context, "target", "")
    val index = units.indexOfFirst { it.second.equals(target, ignoreCase = true) }.coerceAtLeast(0)
    var choosing by remember { mutableStateOf(false) }
    ListRow(Leading.Glyph(R.drawable.ic_sl_unit), "Head unit", units[index].first, trailing = { Chevron() }) {
        choosing = true
    }
    if (choosing) {
        ChoiceDialog("Head unit", units.map { it.first }, index, { choosing = false }) {
            Variant.prefs(context).edit().putString("target", units[it].second).apply()
            choosing = false
            onChange()
        }
    }
}

@Composable
fun OnConnectRow() {
    val context = LocalContext.current
    val app = Choices.current(context)
    val glyph = app == Previews.HOME && Choices.homeApp(context) == null
    ListRow(
        Leading.Picture(Choices.icon(context, app), tinted = glyph),
        "On connect", Choices.name(context, app), trailing = { Chevron() },
    ) { context.startActivity(Intent(context, OnConnectActivity::class.java)) }
}

@Composable
fun ScaleRow(onChange: () -> Unit) {
    val context = LocalContext.current
    val density = Variant.setting(context, "density", "")
    val index = (1 until DENSITIES.size).firstOrNull { DENSITIES[it].toString() == density } ?: 0
    var choosing by remember { mutableStateOf(false) }
    ListRow(Leading.Glyph(R.drawable.ic_sl_text_size), "Scale", SIZES[index], trailing = { Chevron() }) {
        choosing = true
    }
    if (choosing) {
        ChoiceDialog("Scale on the head unit", SIZES, index, { choosing = false }) {
            Variant.prefs(context).edit()
                .putString("density", if (DENSITIES[it] == 0) "" else DENSITIES[it].toString())
                .apply()
            choosing = false
            onChange()
        }
    }
}

@Composable
fun RemoteAppsRow() {
    val context = LocalContext.current
    val count = Switchgear.apps(context).size
    ListRow(
        Leading.Glyph(R.drawable.ic_sl_remote), "Remote mode apps",
        if (count == 1) "1 app" else "$count apps",
        trailing = { Chevron() },
    ) { context.startActivity(Intent(context, RemoteAppsActivity::class.java)) }
}
