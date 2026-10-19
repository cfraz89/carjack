package com.carjack;

import android.content.Context;
import android.net.ConnectivityManager;
import android.net.LinkAddress;
import android.net.LinkProperties;
import android.net.MacAddress;
import android.net.Network;
import android.net.NetworkCapabilities;
import android.net.NetworkRequest;
import android.net.wifi.WifiInfo;
import android.net.wifi.WifiManager;
import android.net.wifi.WifiNetworkSpecifier;
import android.os.Handler;
import android.util.Log;

import java.io.IOException;
import java.net.Inet4Address;
import java.net.NetworkInterface;
import java.net.SocketException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Paths;

/**
 * Joins the accessory's access point as a peer-to-peer network and routes this process over it.
 * Where the device runs two STAs (STA+STA local-only concurrency) that is the second one and the
 * primary connection stays up; elsewhere the framework moves its one radio over. Either way only
 * this process uses the accessory's network, which has no internet.
 */
final class AccessoryWifi {
    private static final String TAG = "carjack";
    private static final int JOIN_TIMEOUT_MS = 30000;
    /** What Android hands apps in place of a MAC address they may not see. */
    private static final String HIDDEN_MAC = "02:00:00:00:00:00";

    interface Result {
        void joined(String iface, String mac);

        void failed(String reason);
    }

    private final Handler handler;
    private final WifiManager wifi;
    private final ConnectivityManager connectivity;
    private WifiManager.MulticastLock multicast;
    private ConnectivityManager.NetworkCallback callback;
    private Result pending;

    AccessoryWifi(Context context, Handler handler) {
        this.handler = handler;
        this.wifi = context.getSystemService(WifiManager.class);
        this.connectivity = context.getSystemService(ConnectivityManager.class);
    }

    /** Runs on the handler; {@code result} is answered there once. */
    void join(String ssid, String passphrase, String bssid, Result result) {
        leave();
        pending = result;
        if (!wifi.isWifiEnabled()) {
            wifi.setWifiEnabled(true);
        }

        WifiNetworkSpecifier.Builder spec = new WifiNetworkSpecifier.Builder().setSsid(ssid);
        if (passphrase != null && !passphrase.isEmpty()) {
            spec.setWpa2Passphrase(passphrase);
        }
        if (bssid != null) {
            try {
                MacAddress address = MacAddress.fromString(bssid);
                // The CFMOTO sends a multicast one (3D:...), no AP's; Android refuses a request
                // pinned to it outright, so the SSID alone has to do.
                if (address.getAddressType() == MacAddress.TYPE_UNICAST) {
                    spec.setBssid(address);
                } else {
                    Log.w(TAG, "Ignoring the accessory's BSSID " + bssid + ", which no AP has");
                }
            } catch (IllegalArgumentException e) {
                Log.w(TAG, "Ignoring the accessory's BSSID " + bssid);
            }
        }
        NetworkRequest request = new NetworkRequest.Builder()
                .addTransportType(NetworkCapabilities.TRANSPORT_WIFI)
                .removeCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
                .setNetworkSpecifier(spec.build())
                .build();
        Log.i(TAG, "Joining " + ssid);
        callback = new ConnectivityManager.NetworkCallback(
                ConnectivityManager.NetworkCallback.FLAG_INCLUDE_LOCATION_INFO) {
            @Override
            public void onCapabilitiesChanged(Network network, NetworkCapabilities caps) {
                check(network, caps, connectivity.getLinkProperties(network));
            }

            @Override
            public void onLinkPropertiesChanged(Network network, LinkProperties link) {
                check(network, connectivity.getNetworkCapabilities(network), link);
            }

            @Override
            public void onUnavailable() {
                answer(null, "the accessory's WiFi did not come up");
            }

            @Override
            public void onLost(Network network) {
                Log.w(TAG, "Lost the accessory's WiFi");
            }
        };
        connectivity.requestNetwork(request, callback, handler, JOIN_TIMEOUT_MS);
    }

    private void check(Network network, NetworkCapabilities caps, LinkProperties link) {
        if (pending == null || caps == null || link == null) {
            return;
        }
        String iface = link.getInterfaceName();
        if (iface == null || !hasUsableAddress(link)) {
            return;
        }
        connectivity.bindProcessToNetwork(network);
        multicast = wifi.createMulticastLock("carjack");
        multicast.acquire();
        WifiInfo info = caps.getTransportInfo() instanceof WifiInfo w ? w : null;
        answer(iface, mac(iface, info));
    }

    /** IPv4, or an IPv6 link-local one: the accessory's AirPlay can live on either. */
    private static boolean hasUsableAddress(LinkProperties link) {
        for (LinkAddress address : link.getLinkAddresses()) {
            if (address.getAddress() instanceof Inet4Address
                    || address.getAddress().isLinkLocalAddress()) {
                return true;
            }
        }
        return false;
    }

    /** The address the interface joined with, which is randomised per network. */
    private static String mac(String iface, WifiInfo info) {
        try {
            String mac = new String(Files.readAllBytes(Paths.get("/sys/class/net", iface, "address")),
                    StandardCharsets.US_ASCII).trim();
            if (!mac.isEmpty()) {
                return mac.toUpperCase();
            }
        } catch (IOException | SecurityException ignored) {
        }
        if (info != null && info.getMacAddress() != null
                && !HIDDEN_MAC.equals(info.getMacAddress())) {
            return info.getMacAddress().toUpperCase();
        }
        try {
            NetworkInterface nif = NetworkInterface.getByName(iface);
            byte[] hw = nif == null ? null : nif.getHardwareAddress();
            if (hw != null && hw.length == 6) {
                return String.format("%02X:%02X:%02X:%02X:%02X:%02X",
                        hw[0], hw[1], hw[2], hw[3], hw[4], hw[5]);
            }
        } catch (SocketException ignored) {
        }
        String mac = Variant.interfaceMac(iface);
        if (mac != null) {
            return mac.toUpperCase();
        }
        Log.w(TAG, "No MAC address for " + iface);
        return HIDDEN_MAC;
    }

    private void answer(String iface, String detail) {
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

    /** Release the accessory's network; the framework drops the connection with the request. */
    void leave() {
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
    }
}
