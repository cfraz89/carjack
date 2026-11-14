package io.github.cfraz89.slipstream

import android.Manifest
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
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import rikka.shizuku.Shizuku

/** What's left to set up, and the few choices there are; opened from [StartActivity]. */
class SettingsActivity : ComponentActivity() {

    companion object {
        private const val REQUEST_PERMISSIONS = 1
        private const val REQUEST_SHIZUKU = 4
        private const val DISCOVERABLE_SECONDS = 300
        private const val SHIZUKU_PACKAGE = "moe.shizuku.privileged.api"
        private const val TAILSCALE_PACKAGE = "com.tailscale.ipn"

        val PERMISSIONS = arrayOf(
            Manifest.permission.BLUETOOTH_CONNECT,
            Manifest.permission.BLUETOOTH_SCAN,
            Manifest.permission.BLUETOOTH_ADVERTISE,
            Manifest.permission.NEARBY_WIFI_DEVICES,
            Manifest.permission.POST_NOTIFICATIONS,
            // The phone's wallpaper, for the launcher on the head unit.
            Manifest.permission.READ_MEDIA_IMAGES,
        )

        /** Granted with the rest through Shizuku, but nothing waits on it: the weather. */
        private val OPTIONAL_PERMISSIONS = arrayOf(
            Manifest.permission.ACCESS_COARSE_LOCATION,
        )
    }

    private enum class Dialog { LOGS }

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

    /** Whether a unit's link is up, for the Bluetooth shortcut. */
    private val prefsChanged = SharedPreferences.OnSharedPreferenceChangeListener { _, key ->
        if (key == "linked") {
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
        revision++
        autoSetUp()
    }

    @Composable
    private fun Screen() = key(revision) {
        Page {
            TopBar("Settings", ::finish)
            val steps = setupSteps()
            if (steps.isNotEmpty()) {
                SetupCard(steps)
            }
            // Pairing has a card of its own, once there's Bluetooth to pair with.
            if (permissionsGranted() && bluetoothOn() && pairedUnits(this@SettingsActivity).size <= 1) {
                Spacer(Modifier.height(12.dp))
                PairCard()
            }
            Heading("Connection")
            SettingsCard()
            Heading("Help")
            HelpCard()
        }
        when (dialog) {
            Dialog.LOGS -> SendLogsDialog()
            null -> {}
        }
        help?.let { HelpScreen(it) { help = null } }
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
        // An app inside a VPN can't reach the head unit's Wi-Fi: the stream times out.
        if (inVpn()) {
            // Excluding an app is the VPN app's own setting; Android's VPN page only has on/off.
            val tailscale = packageManager.getLaunchIntentForPackage(TAILSCALE_PACKAGE)
            steps += if (tailscale != null) {
                Step("Tailscale blocks the head unit: exclude Slipstream in split tunneling",
                    "Tailscale", SetupHelp.tailscale) { startActivity(tailscale) }
            } else {
                Step("A VPN blocks the head unit: exclude Slipstream, or turn it off", "VPN", SetupHelp.vpn) {
                    startActivity(Intent(Settings.ACTION_VPN_SETTINGS))
                }
            }
        }
        return steps
    }

    /** First-time pairing, step by step: the head unit searches, the phone is found, codes match. */
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
                "On the head unit, add a phone",
                "Make this phone visible",
                "Pick it on the head unit, and confirm the code",
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

    /** Each a row showing its current value, opening its choices. */
    @Composable
    private fun SettingsCard() {
        val context = this
        SectionCard(MaterialTheme.colorScheme.surfaceContainer) {
            HeadUnitRow { revision++ }
            OnConnectRow()

            ListRow(Leading.Glyph(R.drawable.ic_sl_home), "Preview the launcher",
                "Try or arrange it here", trailing = { Chevron() }) {
                startActivity(Intent().setClassName(context, "io.github.cfraz89.slipstream.LauncherOnPhone"))
            }

            ScaleRow { revision++ }
            RemoteAppsRow()

            val navBar = Variant.setting(context, "nav_bar", "false").toBoolean()
            ListRow(
                Leading.Glyph(R.drawable.ic_sl_home), "Back and home bar",
                "Along the bottom of the head unit. From next session", trailing = { RowSwitch(navBar) },
            ) {
                Variant.prefs(context).edit().putString("nav_bar", (!navBar).toString()).apply()
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
                        "A few days of logs, with head unit names and Bluetooth addresses." +
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
