package io.github.cfraz89.slipstream

import android.content.Context
import android.net.ConnectivityManager
import android.net.LinkProperties
import android.net.MacAddress
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.net.wifi.WifiInfo
import android.net.wifi.WifiManager
import android.net.wifi.WifiNetworkSpecifier
import android.os.Handler
import android.util.Log
import java.io.File
import java.io.IOException
import java.net.Inet4Address
import java.net.NetworkInterface
import java.net.SocketException
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors

/**
 * Joins the accessory's access point and routes this process over it. Where the variant can
 * (Shizuku), the phone's WiFi switches over to it, so the radio isn't time-slicing with another
 * network on another channel; the phone's own traffic stays on mobile data, its default while this
 * network has no internet. Otherwise it's a local-only second network: with STA+STA concurrency
 * the main connection stays up, elsewhere the framework moves its one radio over. Either way only
 * this process uses the accessory's network.
 */
class AccessoryWifi(context: Context, private val handler: Handler) {
    interface Result {
        fun joined(iface: String, mac: String)

        fun failed(reason: String)
    }

    companion object {
        private const val TAG = "carjack"
        private const val JOIN_TIMEOUT_MS = 30000
        /** What Android hands apps in place of a MAC address they may not see. */
        private const val HIDDEN_MAC = "02:00:00:00:00:00"

        /** Shell calls, in order: a leave must never land after the next join. */
        private val SHELL: ExecutorService = Executors.newSingleThreadExecutor()

        /** Runs [then] once the shell calls queued so far are done. */
        fun afterShell(then: Runnable) {
            SHELL.execute(then)
        }

        /** Unlocks network switching, which a session cut short (a crash) may have left locked. */
        fun reset() {
            SHELL.execute { Variant.leavePrimary(null) }
        }

        /** IPv4, or an IPv6 link-local one: the accessory's AirPlay can live on either. */
        private fun hasUsableAddress(link: LinkProperties) = link.linkAddresses.any {
            it.address is Inet4Address || it.address.isLinkLocalAddress
        }

        /** The address the interface joined with, which is randomised per network. */
        private fun mac(iface: String, info: WifiInfo?): String {
            try {
                val mac = File("/sys/class/net/$iface/address").readText(Charsets.US_ASCII).trim()
                if (mac.isNotEmpty()) return mac.uppercase()
            } catch (ignored: IOException) {
            } catch (ignored: SecurityException) {
            }
            val reported = info?.macAddress
            if (reported != null && reported != HIDDEN_MAC) return reported.uppercase()
            try {
                val hw = NetworkInterface.getByName(iface)?.hardwareAddress
                if (hw != null && hw.size == 6) {
                    return hw.joinToString(":") { "%02X".format(it) }
                }
            } catch (ignored: SocketException) {
            }
            Variant.interfaceMac(iface)?.let { return it.uppercase() }
            Log.w(TAG, "No MAC address for $iface")
            return HIDDEN_MAC
        }
    }

    private val wifi = context.getSystemService(WifiManager::class.java)
    private val connectivity = context.getSystemService(ConnectivityManager::class.java)
    private var multicast: WifiManager.MulticastLock? = null
    private var callback: ConnectivityManager.NetworkCallback? = null
    private var pending: Result? = null
    /** The unit's AP while the phone's WiFi is switched over to it, for leave() to undo. */
    private var primarySsid: String? = null

    /** Runs on the handler; [result] is answered there once. */
    fun join(ssid: String, passphrase: String?, bssid: String?, result: Result) {
        leave()
        pending = result
        @Suppress("DEPRECATION")
        if (!wifi.isWifiEnabled) wifi.isWifiEnabled = true
        // The phone's WiFi switched over where the variant can, else a local-only second network.
        SHELL.execute {
            val primary = Variant.joinPrimary(ssid, passphrase)
            handler.post {
                if (pending !== result) {
                    if (primary) SHELL.execute { Variant.leavePrimary(ssid) }
                } else if (primary) {
                    primarySsid = ssid
                    Log.i(TAG, "Switched the phone's WiFi over to $ssid")
                    request(
                        NetworkRequest.Builder()
                            .addTransportType(NetworkCapabilities.TRANSPORT_WIFI)
                            .removeCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
                            .build(),
                    )
                } else {
                    joinLocalOnly(ssid, passphrase, bssid)
                }
            }
        }
    }

    private fun joinLocalOnly(ssid: String, passphrase: String?, bssid: String?) {
        val spec = WifiNetworkSpecifier.Builder().setSsid(ssid)
        if (!passphrase.isNullOrEmpty()) spec.setWpa2Passphrase(passphrase)
        if (bssid != null) {
            try {
                val address = MacAddress.fromString(bssid)
                // The CFMOTO sends a multicast one (3D:...), no AP's; Android refuses a request
                // pinned to it outright, so the SSID alone has to do.
                if (address.addressType == MacAddress.TYPE_UNICAST) {
                    spec.setBssid(address)
                } else {
                    Log.w(TAG, "Ignoring the accessory's BSSID $bssid, which no AP has")
                }
            } catch (e: IllegalArgumentException) {
                Log.w(TAG, "Ignoring the accessory's BSSID $bssid")
            }
        }
        Log.i(TAG, "Joining $ssid alongside the phone's own WiFi")
        request(
            NetworkRequest.Builder()
                .addTransportType(NetworkCapabilities.TRANSPORT_WIFI)
                .removeCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
                .setNetworkSpecifier(spec.build())
                .build(),
        )
    }

    /**
     * Holds [request] for the session: switched over, it also keeps the framework from tearing
     * down a network nothing else wants, having no internet.
     */
    private fun request(request: NetworkRequest) {
        val cb = object : ConnectivityManager.NetworkCallback(FLAG_INCLUDE_LOCATION_INFO) {
            override fun onCapabilitiesChanged(network: Network, caps: NetworkCapabilities) {
                check(network, caps, connectivity.getLinkProperties(network))
            }

            override fun onLinkPropertiesChanged(network: Network, link: LinkProperties) {
                check(network, connectivity.getNetworkCapabilities(network), link)
            }

            override fun onUnavailable() {
                answer(null, "the accessory's WiFi did not come up")
            }

            override fun onLost(network: Network) {
                Log.w(TAG, "Lost the accessory's WiFi")
            }
        }
        callback = cb
        connectivity.requestNetwork(request, cb, handler, JOIN_TIMEOUT_MS)
    }

    private fun check(network: Network, caps: NetworkCapabilities?, link: LinkProperties?) {
        if (pending == null || caps == null || link == null) return
        val iface = link.interfaceName ?: return
        if (!hasUsableAddress(link)) return
        connectivity.bindProcessToNetwork(network)
        multicast = wifi.createMulticastLock("carjack").also { it.acquire() }
        answer(iface, mac(iface, caps.transportInfo as? WifiInfo))
    }

    private fun answer(iface: String?, detail: String) {
        val result = pending ?: return
        pending = null
        if (iface != null) {
            Log.i(TAG, "On the accessory's WiFi: $iface $detail")
            result.joined(iface, detail)
        } else {
            Log.w(TAG, "WiFi join failed: $detail")
            result.failed(detail)
        }
    }

    /** Release the accessory's network; the framework drops the connection with the request. */
    fun leave() {
        pending = null
        connectivity.bindProcessToNetwork(null)
        multicast?.release()
        multicast = null
        callback?.let { connectivity.unregisterNetworkCallback(it) }
        callback = null
        primarySsid?.let { ssid ->
            primarySsid = null
            SHELL.execute { Variant.leavePrimary(ssid) }
        }
    }
}
