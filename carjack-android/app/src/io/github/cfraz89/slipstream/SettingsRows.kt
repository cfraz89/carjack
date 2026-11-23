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

/** Each paired device that hosts CarPlay, as name and address. */
fun pairedDevices(context: Context): List<Pair<String, String>> {
    val devices = mutableListOf<Pair<String, String>>()
    val adapter = context.getSystemService(BluetoothManager::class.java).adapter
    if (adapter?.isEnabled == true &&
        context.checkSelfPermission(Manifest.permission.BLUETOOTH_CONNECT) == PackageManager.PERMISSION_GRANTED
    ) {
        for (device in adapter.bondedDevices) {
            if (Bluetooth.hostsIap(device.uuids)) devices += (device.name ?: device.address) to device.address
        }
    }
    return devices
}

/** A device setting as it's shown: the device's name where it's paired. */
fun deviceName(target: String, devices: List<Pair<String, String>>) = when (target) {
    Profiles.AUTO_SET -> "Auto-set"
    Profiles.ANY -> "Any"
    else -> devices.firstOrNull { it.second.equals(target, ignoreCase = true) }?.first ?: target
}

/** Which device the profile connects to: Auto-set, Any, or a paired one. */
@Composable
fun DeviceRow(onChange: () -> Unit) {
    val context = LocalContext.current
    val devices = remember { pairedDevices(context) }
    val target = Profiles.targetSetting(context, Profiles.selected(context).id)
    val options = buildList {
        add(Profiles.AUTO_SET)
        add(Profiles.ANY)
        devices.mapTo(this) { it.second }
        // One set that's since been unpaired, so it still shows.
        if (none { it.equals(target, ignoreCase = true) }) add(target)
    }
    val index = options.indexOfFirst { it.equals(target, ignoreCase = true) }
    var choosing by remember { mutableStateOf(false) }
    ListRow(Leading.Glyph(R.drawable.ic_sl_bluetooth), deviceName(target, devices), trailing = { Chevron() }) {
        choosing = true
    }
    if (choosing) {
        ChoiceDialog("Device", options.map { deviceName(it, devices) }, index, { choosing = false }) {
            Variant.prefs(context).edit().putString(Profiles.TARGET, options[it]).apply()
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
        "Home screen", Choices.name(context, app), trailing = { Chevron() },
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
