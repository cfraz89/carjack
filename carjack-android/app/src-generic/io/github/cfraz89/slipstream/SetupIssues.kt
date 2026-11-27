package io.github.cfraz89.slipstream

import android.bluetooth.BluetoothManager
import android.content.Context
import android.content.pm.PackageManager
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import io.github.cfraz89.slipstream.aa.AndroidAuto
import rikka.shizuku.Shizuku

/** What's keeping Slipstream from a session, as settings and the connection screen list them. */
enum class SetupIssue(val text: String) {
    SHIZUKU_MISSING("Shizuku not installed"),
    SHIZUKU_STOPPED("Shizuku not running"),
    SHIZUKU_DENIED("Shizuku not allowed"),
    ANDROID_AUTO_MISSING("Android Auto not installed"),
    ANDROID_AUTO_SERVER("Android Auto server not running"),
    PERMISSIONS("Slipstream needs permissions"),
    BLUETOOTH_OFF("Bluetooth is off"),
    TAILSCALE("Tailscale blocks the head unit: exclude Slipstream in split tunneling"),
    VPN("A VPN blocks the head unit: exclude Slipstream, or turn it off"),
    ;

    companion object {
        const val SHIZUKU_PACKAGE = "moe.shizuku.privileged.api"
        const val TAILSCALE_PACKAGE = "com.tailscale.ipn"

        fun of(context: Context): List<SetupIssue> {
            val issues = mutableListOf<SetupIssue>()
            val pm = context.packageManager
            when {
                pm.getLaunchIntentForPackage(SHIZUKU_PACKAGE) == null -> issues += SHIZUKU_MISSING
                !pingShizuku() -> issues += SHIZUKU_STOPPED
                !ShizukuDisplay.available() -> issues += SHIZUKU_DENIED
            }
            if (!SlipstreamMode.isOn(context)) {
                if (!AndroidAuto.installed(context)) {
                    issues += ANDROID_AUTO_MISSING
                } else if (AndroidAuto.serverFound(context) == false) {
                    issues += ANDROID_AUTO_SERVER
                }
            }
            // With Shizuku, settings grants these and turns Bluetooth on itself.
            if (!ShizukuDisplay.available()) {
                if (!permissionsGranted(context)) issues += PERMISSIONS
                if (!bluetoothOn(context)) issues += BLUETOOTH_OFF
            }
            // An app inside a VPN can't reach the head unit's Wi-Fi: the stream times out.
            if (inVpn(context)) {
                issues += if (pm.getLaunchIntentForPackage(TAILSCALE_PACKAGE) != null) TAILSCALE else VPN
            }
            return issues
        }

        fun permissionsGranted(context: Context) = SettingsActivity.PERMISSIONS.all {
            context.checkSelfPermission(it) == PackageManager.PERMISSION_GRANTED
        }

        fun bluetoothOn(context: Context): Boolean =
            context.getSystemService(BluetoothManager::class.java).adapter?.isEnabled == true

        private fun inVpn(context: Context): Boolean {
            val cm = context.getSystemService(ConnectivityManager::class.java)
            val caps = cm.getNetworkCapabilities(cm.activeNetwork)
            return caps?.hasTransport(NetworkCapabilities.TRANSPORT_VPN) == true
        }

        fun pingShizuku() = try {
            Shizuku.pingBinder()
        } catch (e: RuntimeException) {
            false
        }
    }
}
