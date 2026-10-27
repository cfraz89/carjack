package io.github.cfraz89.slipstream

import android.Manifest
import android.app.KeyguardManager
import android.app.NotificationManager
import android.bluetooth.BluetoothAdapter
import android.bluetooth.BluetoothManager
import android.content.ActivityNotFoundException
import android.content.ComponentName
import android.content.Intent
import android.content.SharedPreferences
import android.content.pm.PackageManager
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.net.Uri
import android.os.Bundle
import android.provider.Settings
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.wrapContentSize
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import rikka.shizuku.Shizuku

/** On/off, what's left to set up, and the few choices there are. */
class MainActivity : ComponentActivity() {

    private companion object {
        const val REQUEST_PERMISSIONS = 1
        const val REQUEST_SHIZUKU = 4
        const val DISCOVERABLE_SECONDS = 300
        const val SHIZUKU_PACKAGE = "moe.shizuku.privileged.api"
        const val TAILSCALE_PACKAGE = "com.tailscale.ipn"

        val PERMISSIONS = arrayOf(
            Manifest.permission.BLUETOOTH_CONNECT,
            Manifest.permission.BLUETOOTH_SCAN,
            Manifest.permission.BLUETOOTH_ADVERTISE,
            Manifest.permission.NEARBY_WIFI_DEVICES,
            Manifest.permission.POST_NOTIFICATIONS,
            // The phone's wallpaper, for the launcher on the unit.
            Manifest.permission.READ_MEDIA_IMAGES,
        )

        /** Granted with the rest through Shizuku, but nothing waits on them: the weather, here. */
        val OPTIONAL_PERMISSIONS = arrayOf(Manifest.permission.ACCESS_COARSE_LOCATION)

        /** Display densities offered, by name; 0 is automatic. */
        val SIZES = listOf("Default", "Smaller", "Larger", "Largest")
        val DENSITIES = intArrayOf(0, 160, 240, 280)
    }

    private enum class Dialog { UNIT, SIZE, LOGS }

    /** A thing still to set up, with the button that sees to it and how to do it by hand. */
    private data class Step(
        val text: String,
        val action: String,
        val help: Help,
        val onClick: () -> Unit,
    )

    private var help by mutableStateOf<Help?>(null)

    /** Bumped by [refresh]: the screen reads everything afresh from settings and the system. */
    private var revision by mutableIntStateOf(0)
    private var dialog by mutableStateOf<Dialog?>(null)
    private var unitNames = listOf("Any")
    private var unitAddresses = listOf("")
    private var settingUp = false
    private var optionalTried = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.setDecorFitsSystemWindows(false)
        Shizuku.addRequestPermissionResultListener(shizukuResult)
        Variant.prefs(this).registerOnSharedPreferenceChangeListener(prefsChanged)
        setContent { SlipstreamTheme { Screen() } }
    }

    private val shizukuResult =
        Shizuku.OnRequestPermissionResultListener { _, _ -> runOnUiThread(::refresh) }

    /** The service's status, and the switch flipped from the notification. */
    private val prefsChanged = SharedPreferences.OnSharedPreferenceChangeListener { _, key ->
        if (key in setOf("status", "on", "retryable", "linked", "trust_seen", Variant.POWER_LOCKS)) {
            runOnUiThread { revision++ }
        }
    }

    override fun onDestroy() {
        Shizuku.removeRequestPermissionResultListener(shizukuResult)
        Variant.prefs(this).unregisterOnSharedPreferenceChangeListener(prefsChanged)
        super.onDestroy()
    }

    override fun onResume() {
        super.onResume()
        refresh()
    }

    @Deprecated("Deprecated in Java")
    override fun onRequestPermissionsResult(
        requestCode: Int,
        permissions: Array<String>,
        grantResults: IntArray,
    ) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        refresh()
    }

    private fun refresh() {
        findUnits()
        revision++
        autoSetUp()
    }

    @Composable
    private fun Screen() = key(revision) {
        Page {
            Header()
            val steps = setupSteps()
            if (steps.isNotEmpty()) {
                Spacer(Modifier.height(12.dp))
                SetupCard(steps)
            }
            // Pairing has a card of its own, once there's Bluetooth to pair with.
            if (permissionsGranted() && bluetoothOn() && unitAddresses.size <= 1) {
                Spacer(Modifier.height(12.dp))
                PairCard()
            }
            Spacer(Modifier.height(12.dp))
            PowerCard()
            Heading("Connection")
            SettingsCard()
            Heading("Help")
            HelpCard()
        }
        when (dialog) {
            Dialog.UNIT -> ChoiceDialog(
                "Unit", unitNames, unitIndex(), { dialog = null },
            ) {
                Variant.prefs(this).edit().putString("target", unitAddresses[it]).apply()
                dialog = null
                revision++
            }
            Dialog.SIZE -> ChoiceDialog(
                "Scale on the unit", SIZES, densityIndex(), { dialog = null },
            ) {
                Variant.prefs(this).edit()
                    .putString("density", if (DENSITIES[it] == 0) "" else DENSITIES[it].toString())
                    .apply()
                dialog = null
                revision++
            }
            Dialog.LOGS -> SendLogsDialog()
            null -> {}
        }
        help?.let { HelpScreen(it) { help = null } }
    }

    @Composable
    private fun Header() {
        Row(
            Modifier.padding(start = 8.dp, end = 8.dp, top = 24.dp, bottom = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Image(painterResource(R.drawable.ic_sl_app), null, Modifier.size(56.dp))
            Spacer(Modifier.width(16.dp))
            // Yellowtail (Apache 2.0, res/font): a fast script, shaded pink into blue like a trail.
            val colors = MaterialTheme.colorScheme
            Text(
                "Slipstream",
                style = TextStyle(
                    fontFamily = FontFamily(Font(R.font.yellowtail)),
                    fontSize = 40.sp,
                    brush = Brush.horizontalGradient(listOf(colors.tertiary, colors.primary)),
                ),
            )
        }
    }

    /** One line per thing still missing; the card goes when nothing is. */
    @Composable
    private fun SetupCard(steps: List<Step>) {
        val colors = MaterialTheme.colorScheme
        SectionCard(
            colors.tertiaryContainer,
            padding = PaddingValues(start = 20.dp, end = 16.dp, top = 16.dp, bottom = 16.dp),
        ) {
            steps.forEachIndexed { index, step ->
                Row(
                    Modifier.padding(top = if (index == 0) 0.dp else 12.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        step.text,
                        color = colors.onTertiaryContainer,
                        fontSize = 15.sp,
                        modifier = Modifier.weight(1f).padding(end = 4.dp),
                    )
                    IconButton(onClick = { help = step.help }) {
                        Icon(
                            painterResource(R.drawable.ic_sl_info),
                            contentDescription = "How to",
                            tint = colors.onTertiaryContainer,
                        )
                    }
                    FilledButton(step.action, step.onClick)
                }
            }
        }
    }

    private fun setupSteps(): List<Step> {
        val steps = mutableListOf<Step>()
        val shizuku = packageManager.getLaunchIntentForPackage(SHIZUKU_PACKAGE)
        if (shizuku == null) {
            steps += Step("Slipstream needs Shizuku", "Install", SetupHelp.installShizuku, ::installShizuku)
        } else if (!pingShizuku()) {
            steps += Step("Start Shizuku", "Open", SetupHelp.startShizuku) { startActivity(shizuku) }
        } else if (!ShizukuDisplay.available()) {
            steps += Step("Slipstream needs Shizuku", "Allow", SetupHelp.allowShizuku) {
                Shizuku.requestPermission(REQUEST_SHIZUKU)
            }
        }
        // With Shizuku, autoSetUp sees to these.
        if (!ShizukuDisplay.available()) {
            if (!permissionsGranted()) {
                steps += Step("Slipstream needs permissions", "Allow", SetupHelp.permissions) {
                    requestPermissions(PERMISSIONS, REQUEST_PERMISSIONS)
                }
            }
            if (!bluetoothOn()) {
                steps += Step("Bluetooth is off", "Turn on", SetupHelp.bluetooth) {
                    startActivity(Intent(BluetoothAdapter.ACTION_REQUEST_ENABLE))
                }
            }
        }
        // Locked, the phone hides every app on the unit but the launcher; a trusted unit keeps it
        // unlocked while connected. Gone once that's been seen working.
        val secure = getSystemService(KeyguardManager::class.java).isDeviceSecure
        if (unitAddresses.size > 1 && !Variant.prefs(this).getBoolean("trust_seen", false) &&
            secure
        ) {
            steps += Step("Make the unit a trusted device, to stay unlocked", "Settings",
                SetupHelp.trustedDevice, ::openSecuritySettings)
        }
        // The lock hold keeps a screen timeout from locking, but not the power button.
        if (secure && Variant.holdsUnlock(this) &&
            Variant.prefs(this).getBoolean(Variant.POWER_LOCKS, false)
        ) {
            steps += Step(
                "The power button locks the phone, hiding the unit's apps: turn off \"Power " +
                    "button instantly locks\"",
                "Settings",
                SetupHelp.powerButton,
                ::openSecuritySettings,
            )
        }
        // An app inside a VPN can't reach the unit's Wi-Fi: the stream times out.
        if (inVpn()) {
            // Excluding an app is the VPN app's own setting; Android's VPN page only has on/off.
            val tailscale = packageManager.getLaunchIntentForPackage(TAILSCALE_PACKAGE)
            steps += if (tailscale != null) {
                Step("Tailscale blocks the unit: exclude Slipstream in split tunneling",
                    "Tailscale", SetupHelp.tailscale) { startActivity(tailscale) }
            } else {
                Step("A VPN blocks the unit: exclude Slipstream, or turn it off", "VPN", SetupHelp.vpn) {
                    startActivity(Intent(Settings.ACTION_VPN_SETTINGS))
                }
            }
        }
        return steps
    }

    /** First-time pairing, step by step: the unit searches, the phone is found, codes match. */
    @Composable
    private fun PairCard() {
        val colors = MaterialTheme.colorScheme
        SectionCard(
            colors.primaryContainer,
            padding = PaddingValues(start = 20.dp, end = 16.dp, top = 16.dp, bottom = 16.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(
                    painterResource(R.drawable.ic_sl_bluetooth), null,
                    Modifier.size(20.dp), tint = colors.onPrimaryContainer,
                )
                Spacer(Modifier.width(10.dp))
                Text(
                    "Pair your head unit",
                    color = colors.onPrimaryContainer,
                    fontSize = 16.sp,
                    fontWeight = FontWeight.Medium,
                )
            }
            listOf(
                "On the unit, add a phone",
                "Make this phone visible",
                "Pick it on the unit, and confirm the code",
            ).forEachIndexed { index, step ->
                Row(Modifier.padding(top = 10.dp), verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        "${index + 1}",
                        color = colors.onTertiary,
                        fontSize = 13.sp,
                        modifier = Modifier
                            .size(22.dp)
                            .background(colors.tertiary, CircleShape)
                            .wrapContentSize(Alignment.Center),
                    )
                    Spacer(Modifier.width(12.dp))
                    Text(step, color = colors.onPrimaryContainer, fontSize = 15.sp)
                }
            }
            Row(Modifier.padding(top = 14.dp)) {
                FilledButton("Make visible") {
                    startActivity(
                        Intent(BluetoothAdapter.ACTION_REQUEST_DISCOVERABLE)
                            .putExtra(BluetoothAdapter.EXTRA_DISCOVERABLE_DURATION, DISCOVERABLE_SECONDS),
                    )
                }
                PlainButton("Bluetooth settings") {
                    startActivity(Intent(Settings.ACTION_BLUETOOTH_SETTINGS))
                }
            }
        }
    }

    @Composable
    private fun PowerCard() {
        val on = BootReceiver.isOn(this)
        val colors = MaterialTheme.colorScheme
        SectionCard(if (on) colors.primaryContainer else colors.surfaceContainerHigh) {
            ListRow(
                Leading.Glyph(R.drawable.ic_sl_cast),
                "Enabled",
                if (on) Variant.setting(this@MainActivity, "status", SlipstreamService.WAITING) else "Off",
                titleSize = 20,
                trailing = { RowSwitch(on) },
            ) { setOn(!on) }
            if (on && Variant.prefs(this@MainActivity).getBoolean("retryable", false)) {
                Row(Modifier.padding(start = 72.dp, end = 16.dp, bottom = 8.dp)) {
                    FilledButton("Retry") {
                        startService(
                            Intent(this@MainActivity, SlipstreamService::class.java)
                                .setAction(SlipstreamService.ACTION_RETRY),
                        )
                    }
                }
            }
        }
    }

    /** Each a row showing its current value, opening its choices. */
    @Composable
    private fun SettingsCard() {
        val context = this
        SectionCard(MaterialTheme.colorScheme.surfaceContainer) {
            ListRow(Leading.Glyph(R.drawable.ic_sl_unit), "Unit", unitNames[unitIndex()],
                trailing = { Chevron() }) { dialog = Dialog.UNIT }

            val app = Choices.current(context)
            val glyph = app == Previews.HOME && Variant.homeApp(context) == null
            ListRow(
                Leading.Picture(Choices.icon(context, app), tinted = glyph),
                "On connect", Choices.name(context, app), trailing = { Chevron() },
            ) { startActivity(Intent(context, OnConnectActivity::class.java)) }

            ListRow(Leading.Glyph(R.drawable.ic_sl_home), "Preview the launcher",
                "Try or arrange it here", trailing = { Chevron() }) {
                startActivity(Intent().setClassName(context, "io.github.cfraz89.slipstream.LauncherOnPhone"))
            }

            ListRow(Leading.Glyph(R.drawable.ic_sl_text_size), "Scale", SIZES[densityIndex()],
                trailing = { Chevron() }) { dialog = Dialog.SIZE }

            val remoteApps = Switchgear.apps(context).size
            ListRow(
                Leading.Glyph(R.drawable.ic_sl_remote), "Remote mode apps",
                if (remoteApps == 1) "1 app" else "$remoteApps apps",
                trailing = { Chevron() },
            ) { startActivity(Intent(context, RemoteAppsActivity::class.java)) }

            val dark = Variant.keepsPanel(context)
            ListRow(
                Leading.Glyph(R.drawable.ic_sl_cast), "Dark screen",
                "The phone's screen goes dark instead of sleeping", trailing = { RowSwitch(dark) },
            ) {
                Variant.prefs(context).edit().putBoolean("keep_panel", !dark).apply()
                revision++
            }

            val navBar = Variant.setting(context, "nav_bar", "false").toBoolean()
            ListRow(
                Leading.Glyph(R.drawable.ic_sl_home), "Back and home bar",
                "Along the bottom of the unit. From next session", trailing = { RowSwitch(navBar) },
            ) {
                Variant.prefs(context).edit().putString("nav_bar", (!navBar).toString()).apply()
                revision++
            }

            val unlocked = Variant.holdsUnlock(context)
            ListRow(
                Leading.Glyph(R.drawable.ic_sl_lock_open), "Stay unlocked while connected",
                trailing = { RowSwitch(unlocked) },
            ) {
                Variant.prefs(context).edit().putBoolean("hold_unlock", !unlocked).apply()
                Variant.applyLockHold(context)
                revision++
            }
        }
    }

    @Composable
    private fun HelpCard() {
        val context = this
        SectionCard(MaterialTheme.colorScheme.surfaceContainer) {
            ListRow(Leading.Glyph(R.drawable.ic_sl_share), "Send logs", "Report a problem") {
                dialog = Dialog.LOGS
            }
            if (Reports.configured(context)) {
                val reports = Variant.prefs(context).getBoolean(Reports.PREF, true)
                ListRow(
                    Leading.Glyph(R.drawable.ic_sl_shield), "Send crash reports",
                    "Automatically, through Sentry", trailing = { RowSwitch(reports) },
                ) {
                    Variant.prefs(context).edit().putBoolean(Reports.PREF, !reports).apply()
                    Reports.apply(context)
                    revision++
                }
            }
        }
    }

    /** The logs say more than their name suggests, so the sender knows before they go. */
    @Composable
    private fun SendLogsDialog() {
        val upload = remember { Reports.on() }
        var description by remember { mutableStateOf("") }
        AlertDialog(
            onDismissRequest = { dialog = null },
            title = { Text("Send logs?") },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(16.dp)) {
                    Text(
                        "A few days of logs, with unit names and Bluetooth addresses." +
                            if (upload) "" else " Send only to whoever's looking into it.",
                    )
                    if (upload) {
                        OutlinedTextField(
                            description,
                            { description = it },
                            placeholder = { Text("What happened, and when?") },
                            modifier = Modifier.fillMaxWidth(),
                        )
                    }
                }
            },
            confirmButton = {
                TextButton(onClick = {
                    dialog = null
                    sendLogs(if (upload) description.trim() else "", !upload)
                }) { Text("Send") }
            },
            dismissButton = {
                Row {
                    if (upload) {
                        TextButton(onClick = {
                            dialog = null
                            sendLogs(description.trim(), true)
                        }) { Text("Email") }
                    }
                    TextButton(onClick = { dialog = null }) { Text("Cancel") }
                }
            },
        )
    }

    private fun sendLogs(description: String, email: Boolean) {
        Toast.makeText(this, "Gathering logs…", Toast.LENGTH_SHORT).show()
        Thread({
            try {
                val share = LogBundle.send(this, description, email)
                runOnUiThread {
                    if (share != null) {
                        startActivity(share)
                    } else {
                        Toast.makeText(this, "Logs sent. Thanks!", Toast.LENGTH_SHORT).show()
                    }
                }
            } catch (e: java.io.IOException) {
                runOnUiThread {
                    Toast.makeText(this, "Couldn't gather the logs: ${e.message}", Toast.LENGTH_LONG)
                        .show()
                }
            }
        }, "slipstream-logs").start()
    }

    /** Trusted devices and the power button's lock are both reached from here. */
    private fun openSecuritySettings() {
        startActivity(Intent(Settings.ACTION_SECURITY_SETTINGS))
    }

    /** Shizuku's Play Store page, or its download page where there's no store. */
    private fun installShizuku() {
        try {
            startActivity(Intent(Intent.ACTION_VIEW, Uri.parse("market://details?id=$SHIZUKU_PACKAGE")))
        } catch (e: ActivityNotFoundException) {
            startActivity(Intent(Intent.ACTION_VIEW, Uri.parse("https://shizuku.rikka.app/download/")))
        }
    }

    /** With Shizuku: grant what's missing and switch Bluetooth on, unasked. */
    private fun autoSetUp() {
        val grant = !permissionsGranted()
        // Tried once a visit: refresh() follows, and one refused mustn't loop.
        // The launcher's now-playing panel needs a notification listener to see media sessions.
        val media = ComponentName(this, "io.github.cfraz89.slipstream.launcher.MediaListener")
        val listener = !getSystemService(NotificationManager::class.java)
            .isNotificationListenerAccessGranted(media)
        val optional = !optionalTried && (!granted(OPTIONAL_PERMISSIONS) || listener)
        val bluetooth = !bluetoothOn()
        if (settingUp || !(grant || optional || bluetooth) || !ShizukuDisplay.available()) return
        settingUp = true
        optionalTried = true
        Thread({
            ShizukuDisplay.setUp(packageName, PERMISSIONS + OPTIONAL_PERMISSIONS, bluetooth,
                if (listener) media.flattenToString() else null)
            runOnUiThread {
                settingUp = false
                refresh()
            }
        }, "slipstream-setup").start()
    }

    private fun setOn(wanted: Boolean) {
        var on = wanted
        // A connected-device foreground service can't start before Bluetooth is granted.
        if (on && !permissionsGranted()) {
            requestPermissions(PERMISSIONS, REQUEST_PERMISSIONS)
            on = false
        }
        BootReceiver.setOn(this, on)
        val service = Intent(this, SlipstreamService::class.java)
        if (on) startForegroundService(service) else stopService(service)
        revision++
    }

    /** Automatic, then each paired head unit. */
    private fun findUnits() {
        val names = mutableListOf("Any")
        val addresses = mutableListOf("")
        val adapter = getSystemService(BluetoothManager::class.java).adapter
        if (bluetoothOn() &&
            checkSelfPermission(Manifest.permission.BLUETOOTH_CONNECT) == PackageManager.PERMISSION_GRANTED
        ) {
            for (device in adapter.bondedDevices) {
                if (Bluetooth.hostsIap(device.uuids)) {
                    names += device.name ?: device.address
                    addresses += device.address
                }
            }
        }
        unitNames = names
        unitAddresses = addresses
    }

    private fun unitIndex() = unitAddresses
        .indexOfFirst { it.equals(Variant.setting(this, "target", ""), ignoreCase = true) }
        .coerceAtLeast(0)

    private fun densityIndex(): Int {
        val density = Variant.setting(this, "density", "")
        return (1 until DENSITIES.size).firstOrNull { DENSITIES[it].toString() == density } ?: 0
    }

    private fun permissionsGranted() = granted(PERMISSIONS)

    private fun granted(permissions: Array<String>) =
        permissions.all { checkSelfPermission(it) == PackageManager.PERMISSION_GRANTED }

    private fun bluetoothOn(): Boolean =
        getSystemService(BluetoothManager::class.java).adapter?.isEnabled == true

    /** This app's traffic goes through a VPN. */
    private fun inVpn(): Boolean {
        val cm = getSystemService(ConnectivityManager::class.java)
        val caps = cm.getNetworkCapabilities(cm.activeNetwork)
        return caps?.hasTransport(NetworkCapabilities.TRANSPORT_VPN) == true
    }

    private fun pingShizuku() = try {
        Shizuku.pingBinder()
    } catch (e: RuntimeException) {
        false
    }
}
