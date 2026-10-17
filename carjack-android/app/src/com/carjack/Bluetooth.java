package com.carjack;

import android.bluetooth.BluetoothAdapter;
import android.bluetooth.BluetoothDevice;
import android.bluetooth.BluetoothManager;
import android.bluetooth.BluetoothServerSocket;
import android.bluetooth.BluetoothSocket;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.os.Handler;
import android.os.ParcelUuid;
import android.os.Parcelable;
import android.os.SystemClock;
import android.os.SystemProperties;
import android.util.Log;

import java.io.IOException;
import java.time.Duration;
import java.util.HashMap;
import java.util.Map;
import java.util.UUID;
import java.util.function.Consumer;

/**
 * The Bluetooth side a phone shows a CarPlay unit: always discoverable, pairing without asking,
 * the iAP service the unit dials in on, and paging the unit to get it to dial.
 */
final class Bluetooth {
    private static final String TAG = "carjack";

    static final UUID IAP_DEVICE = UUID.fromString("00000000-deca-fade-deca-deafdecacafe");
    static final UUID IAP_ACCESSORY = UUID.fromString("00000000-deca-fade-deca-deafdecacaff");
    /** Only advertised: units look for it in EIR while bonding. */
    static final UUID CARPLAY_DEVICE = UUID.fromString("2d8d2466-e14d-451c-88bc-7301abea291a");

    private static final long RELISTEN_DELAY_MS = 2000;
    static final long DISCOVERY_INTERVAL_MS = 15000;

    private final Context context;
    private final Handler handler;
    private final BluetoothAdapter adapter;
    private final Consumer<BluetoothSocket> onIap;
    private static final long PROBE_WAIT_MS = 15000;
    private static final long PROBE_RETRY_MS = 60000;

    /** When each device was last asked for its services. */
    private final Map<String, Long> probed = new HashMap<>();
    /** Discovery holds off until then, while a probe is out. */
    private long probeUntil;
    private volatile boolean running;

    Bluetooth(Context context, Handler handler, Consumer<BluetoothSocket> onIap) {
        this.context = context;
        this.handler = handler;
        this.adapter = context.getSystemService(BluetoothManager.class).getAdapter();
        this.onIap = onIap;
    }

    void start() {
        running = true;
        IntentFilter filter = new IntentFilter();
        filter.addAction(BluetoothDevice.ACTION_PAIRING_REQUEST);
        filter.addAction(BluetoothDevice.ACTION_FOUND);
        filter.addAction(BluetoothDevice.ACTION_UUID);
        filter.addAction(BluetoothAdapter.ACTION_STATE_CHANGED);
        filter.setPriority(IntentFilter.SYSTEM_HIGH_PRIORITY);
        context.registerReceiver(receiver, filter, null, handler, Context.RECEIVER_EXPORTED);
        if (!adapter.isEnabled()) {
            adapter.enable();
        } else {
            onEnabled();
        }
    }

    private void onEnabled() {
        // Zero is "never times out".
        adapter.setDiscoverableTimeout(Duration.ZERO);
        adapter.setScanMode(BluetoothAdapter.SCAN_MODE_CONNECTABLE_DISCOVERABLE);
        listen(IAP_DEVICE, "Wireless iAP", onIap);
        listen(CARPLAY_DEVICE, "CarPlay", Bluetooth::closeQuietly);
    }

    String name() {
        return adapter.getName();
    }

    String address() {
        return adapter.getAddress();
    }

    /** Accepts on {@code uuid} until the adapter goes away, then again once it is back. */
    private void listen(UUID uuid, String name, Consumer<BluetoothSocket> onAccept) {
        Thread thread = new Thread(() -> {
            while (running) {
                try (BluetoothServerSocket server =
                        adapter.listenUsingRfcommWithServiceRecord(name, uuid)) {
                    Log.i(TAG, "Listening for " + name);
                    while (running) {
                        BluetoothSocket socket = server.accept();
                        Log.i(TAG, name + " connection from " + socket.getRemoteDevice());
                        onAccept.accept(socket);
                    }
                } catch (IOException e) {
                    Log.w(TAG, name + " listener ended: " + e.getMessage());
                }
                try {
                    Thread.sleep(RELISTEN_DELAY_MS);
                } catch (InterruptedException e) {
                    return;
                }
            }
        }, "carjack-" + name);
        thread.setDaemon(true);
        thread.start();
    }

    /** The configured unit, or else any bonded one that hosts iAP. */
    BluetoothDevice headUnit() {
        if (!adapter.isEnabled()) {
            return null;
        }
        for (BluetoothDevice device : adapter.getBondedDevices()) {
            if (isTarget(device)) {
                return device;
            }
        }
        return null;
    }

    private static String target() {
        String target = SystemProperties.get("persist.carjack.target", "").trim();
        return target.equalsIgnoreCase("auto") ? "" : target;
    }

    private static boolean isNamedTarget(BluetoothDevice device) {
        String target = target();
        return !target.isEmpty() && (target.equalsIgnoreCase(device.getAddress())
                || target.equals(device.getName()));
    }

    private boolean isTarget(BluetoothDevice device) {
        String target = target();
        if (!target.isEmpty()) {
            return target.equalsIgnoreCase(device.getAddress())
                    || target.equals(device.getName());
        }
        return hostsIap(device.getUuids());
    }

    private static boolean hostsIap(Parcelable[] uuids) {
        if (uuids == null) {
            return false;
        }
        for (Parcelable uuid : uuids) {
            if (uuid instanceof ParcelUuid && IAP_ACCESSORY.equals(((ParcelUuid) uuid).getUuid())) {
                return true;
            }
        }
        return false;
    }

    /** Look for an unbonded unit to pair with. */
    void discover() {
        if (adapter.isEnabled() && !adapter.isDiscovering()
                && SystemClock.elapsedRealtime() >= probeUntil) {
            adapter.startDiscovery();
        }
    }

    /**
     * Ask one device for its services. The stack defers SDP behind a running inquiry and drops it
     * when the next one starts, so discovery stops for the duration.
     */
    private void probe(BluetoothDevice device) {
        long now = SystemClock.elapsedRealtime();
        Long last = probed.get(device.getAddress());
        if (now < probeUntil || (last != null && now - last < PROBE_RETRY_MS)) {
            return;
        }
        probed.put(device.getAddress(), now);
        probeUntil = now + PROBE_WAIT_MS;
        Log.i(TAG, "Asking " + device.getAddress() + " (" + device.getName() + ") for its services");
        adapter.cancelDiscovery();
        device.fetchUuidsWithSdp();
    }

    private void pair(BluetoothDevice device) {
        Log.i(TAG, "Found head unit " + device.getAddress() + " (" + device.getName()
                + "); pairing");
        adapter.cancelDiscovery();
        device.createBond();
    }

    /** Drop any link and page again, so the unit offers a fresh iAP channel. */
    void relink(BluetoothDevice device) {
        Log.i(TAG, "Relinking " + device.getAddress());
        if (device.isConnected()) {
            try {
                device.disconnect();
            } catch (RuntimeException e) {
                Log.w(TAG, "Could not disconnect " + device.getAddress(), e);
            }
            handler.postDelayed(() -> connect(device), 1000);
        } else {
            connect(device);
        }
    }

    void page(BluetoothDevice device) {
        if (!device.isConnected()) {
            Log.i(TAG, "Paging " + device.getAddress());
            connect(device);
        }
    }

    private static void connect(BluetoothDevice device) {
        try {
            device.connect();
        } catch (RuntimeException e) {
            Log.w(TAG, "Could not page " + device.getAddress(), e);
        }
    }

    private final BroadcastReceiver receiver = new BroadcastReceiver() {
        @Override
        public void onReceive(Context context, Intent intent) {
            BluetoothDevice device =
                    intent.getParcelableExtra(BluetoothDevice.EXTRA_DEVICE, BluetoothDevice.class);
            switch (intent.getAction()) {
                case BluetoothAdapter.ACTION_STATE_CHANGED:
                    if (intent.getIntExtra(BluetoothAdapter.EXTRA_STATE, -1)
                            == BluetoothAdapter.STATE_ON) {
                        onEnabled();
                    }
                    break;
                case BluetoothDevice.ACTION_PAIRING_REQUEST:
                    confirmPairing(device, intent);
                    if (isOrderedBroadcast()) {
                        abortBroadcast();
                    }
                    break;
                case BluetoothDevice.ACTION_FOUND:
                    if (device.getBondState() != BluetoothDevice.BOND_NONE
                            || device.getType() == BluetoothDevice.DEVICE_TYPE_LE) {
                        break;
                    }
                    if (isNamedTarget(device)) {
                        pair(device);
                    } else if (target().isEmpty()) {
                        probe(device);
                    }
                    break;
                case BluetoothDevice.ACTION_UUID:
                    Parcelable[] uuids = intent.getParcelableArrayExtra(BluetoothDevice.EXTRA_UUID);
                    Log.i(TAG, "Services of " + device.getAddress() + ": "
                            + (uuids == null ? "none" : java.util.Arrays.toString(uuids)));
                    probeUntil = 0;
                    if (device.getBondState() == BluetoothDevice.BOND_NONE && hostsIap(uuids)) {
                        pair(device);
                    }
                    break;
            }
        }
    };

    private static void confirmPairing(BluetoothDevice device, Intent intent) {
        int variant = intent.getIntExtra(BluetoothDevice.EXTRA_PAIRING_VARIANT, -1);
        Log.i(TAG, "Pairing request from " + device.getAddress() + ", variant " + variant);
        switch (variant) {
            case BluetoothDevice.PAIRING_VARIANT_PIN:
                device.setPin("0000");
                break;
            default:
                device.setPairingConfirmation(true);
                break;
        }
    }

    static void closeQuietly(BluetoothSocket socket) {
        try {
            socket.close();
        } catch (IOException ignored) {
        }
    }
}
