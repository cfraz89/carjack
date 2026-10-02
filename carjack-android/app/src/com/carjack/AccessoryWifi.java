package com.carjack;

import android.content.Context;
import android.net.ConnectivityManager;
import android.net.LinkAddress;
import android.net.LinkProperties;
import android.net.Network;
import android.net.NetworkCapabilities;
import android.net.NetworkRequest;
import android.net.wifi.WifiConfiguration;
import android.net.wifi.WifiInfo;
import android.net.wifi.WifiManager;
import android.os.Handler;
import android.util.Log;

import java.net.Inet4Address;
import java.net.NetworkInterface;
import java.net.SocketException;

/**
 * Joins the accessory's access point and routes this process over it. The accessory's network has
 * no internet, so nothing else on the device moves to it.
 */
final class AccessoryWifi {
    private static final String TAG = "carjack";
    private static final long JOIN_TIMEOUT_MS = 30000;

    interface Result {
        void joined(String iface, String mac);

        void failed(String reason);
    }

    private final Handler handler;
    private final WifiManager wifi;
    private final ConnectivityManager connectivity;
    private WifiManager.MulticastLock multicast;
    private ConnectivityManager.NetworkCallback callback;
    private int networkId = -1;
    private Result pending;

    AccessoryWifi(Context context, Handler handler) {
        this.handler = handler;
        this.wifi = context.getSystemService(WifiManager.class);
        this.connectivity = context.getSystemService(ConnectivityManager.class);
    }

    /** Runs on the handler; {@code result} is answered there once. */
    void join(String ssid, String passphrase, Result result) {
        leave();
        pending = result;
        if (!wifi.isWifiEnabled()) {
            wifi.setWifiEnabled(true);
        }

        WifiConfiguration config = new WifiConfiguration();
        config.SSID = "\"" + ssid + "\"";
        if (passphrase == null || passphrase.isEmpty()) {
            config.setSecurityParams(WifiConfiguration.SECURITY_TYPE_OPEN);
        } else {
            config.setSecurityParams(WifiConfiguration.SECURITY_TYPE_PSK);
            config.preSharedKey = "\"" + passphrase + "\"";
        }
        config.allowAutojoin = false;
        networkId = wifi.addNetwork(config);
        Log.i(TAG, "Joining " + ssid + " (network " + networkId + ")");

        NetworkRequest request = new NetworkRequest.Builder()
                .addTransportType(NetworkCapabilities.TRANSPORT_WIFI)
                .removeCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
                .removeCapability(NetworkCapabilities.NET_CAPABILITY_TRUSTED)
                .removeCapability(NetworkCapabilities.NET_CAPABILITY_NOT_RESTRICTED)
                .build();
        callback = new ConnectivityManager.NetworkCallback(
                ConnectivityManager.NetworkCallback.FLAG_INCLUDE_LOCATION_INFO) {
            @Override
            public void onCapabilitiesChanged(Network network, NetworkCapabilities caps) {
                check(network, caps, connectivity.getLinkProperties(network), ssid);
            }

            @Override
            public void onLinkPropertiesChanged(Network network, LinkProperties link) {
                check(network, connectivity.getNetworkCapabilities(network), link, ssid);
            }
        };
        connectivity.requestNetwork(request, callback, handler);
        wifi.connect(networkId, null);
        handler.postDelayed(timeout, JOIN_TIMEOUT_MS);
    }

    private final Runnable timeout = () -> answer(null, "timed out joining the accessory's WiFi");

    private void check(Network network, NetworkCapabilities caps, LinkProperties link, String ssid) {
        if (pending == null || caps == null || link == null) {
            return;
        }
        if (!(caps.getTransportInfo() instanceof WifiInfo info)
                || !("\"" + ssid + "\"").equals(info.getSSID())) {
            return;
        }
        String iface = link.getInterfaceName();
        if (iface == null || !hasUsableAddress(link)) {
            return;
        }
        connectivity.bindProcessToNetwork(network);
        multicast = wifi.createMulticastLock("carjack");
        multicast.acquire();
        answer(iface, mac(iface, info));
    }

    /** IPv4, or an IPv6 link-local one: the accessory's AirPlay can live on either. */
    private static boolean hasUsableAddress(LinkProperties link) {
        for (LinkAddress address : link.getLinkAddresses()) {
            if (address.getAddress() instanceof Inet4Address || address.getAddress().isLinkLocalAddress()) {
                return true;
            }
        }
        return false;
    }

    private static String mac(String iface, WifiInfo info) {
        try {
            NetworkInterface nif = NetworkInterface.getByName(iface);
            byte[] hw = nif == null ? null : nif.getHardwareAddress();
            if (hw != null && hw.length == 6) {
                return String.format("%02X:%02X:%02X:%02X:%02X:%02X",
                        hw[0], hw[1], hw[2], hw[3], hw[4], hw[5]);
            }
        } catch (SocketException ignored) {
        }
        return info.getMacAddress().toUpperCase();
    }

    private void answer(String iface, String detail) {
        handler.removeCallbacks(timeout);
        Result result = pending;
        pending = null;
        if (result == null) {
            return;
        }
        if (iface != null) {
            Log.i(TAG, "On the accessory's WiFi: " + iface + " " + detail);
            result.joined(iface, detail);
        } else {
            Log.w(TAG, "WiFi join failed: " + detail);
            result.failed(detail);
        }
    }

    /** Back off the accessory's network and forget it. */
    void leave() {
        handler.removeCallbacks(timeout);
        pending = null;
        connectivity.bindProcessToNetwork(null);
        if (multicast != null) {
            multicast.release();
            multicast = null;
        }
        if (callback != null) {
            connectivity.unregisterNetworkCallback(callback);
            callback = null;
        }
        if (networkId != -1) {
            wifi.removeNetwork(networkId);
            networkId = -1;
        }
    }
}
