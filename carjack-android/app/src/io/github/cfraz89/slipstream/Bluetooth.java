package io.github.cfraz89.slipstream;

import android.bluetooth.BluetoothAdapter;
import android.bluetooth.BluetoothDevice;
import android.bluetooth.BluetoothManager;
import android.bluetooth.BluetoothProfile;
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
import android.util.Log;

import java.io.IOException;
import java.util.HashMap;
import java.util.Map;
import java.util.UUID;
import java.util.function.Consumer;

/**
 * The Bluetooth side a phone shows a CarPlay unit: discoverable, the iAP service the unit dials in
 * on, and paging the unit to get it to dial. How much of that happens without asking is up to the
 * {@link Variant}.
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
    private volatile BluetoothDevice lastLinked;
    /** The unit's link has just come up: when it dials iAP, if it is going to. */
    private Consumer<BluetoothDevice> onUnitLinked = device -> {};
    private static final long PROBE_WAIT_MS = 15000;
    private static final long PROBE_RETRY_MS = 60000;

    /** When each device was last asked for its services. */
    private final Map<String, Long> probed = new HashMap<>();
    /** Discovery holds off until then, while a probe is out. */
    private long probeUntil;
    private volatile boolean running;
    private final java.util.Set<BluetoothSocket> dialled =
            java.util.concurrent.ConcurrentHashMap.newKeySet();
    /** Open listening sockets, closed to stop their threads. */
    private final java.util.Set<BluetoothServerSocket> servers =
            java.util.concurrent.ConcurrentHashMap.newKeySet();

    Bluetooth(Context context, Handler handler, Consumer<BluetoothSocket> onIap) {
        this.context = context;
        this.handler = handler;
        this.adapter = context.getSystemService(BluetoothManager.class).getAdapter();
        this.onIap = onIap;
    }

    void start() {
        running = true;
        setLinked(null);
        IntentFilter filter = new IntentFilter();
        filter.addAction(BluetoothDevice.ACTION_PAIRING_REQUEST);
        filter.addAction(BluetoothDevice.ACTION_FOUND);
        filter.addAction(BluetoothDevice.ACTION_UUID);
        filter.addAction(BluetoothAdapter.ACTION_STATE_CHANGED);
        filter.addAction(BluetoothDevice.ACTION_ACL_CONNECTED);
        filter.addAction(BluetoothDevice.ACTION_ACL_DISCONNECTED);
        filter.addAction(BluetoothDevice.ACTION_BOND_STATE_CHANGED);
        filter.setPriority(IntentFilter.SYSTEM_HIGH_PRIORITY);
        context.registerReceiver(receiver, filter, null, handler, Context.RECEIVER_EXPORTED);
        // The listeners retry until the adapter is on, which a user may still have to do.
        listen(IAP_DEVICE, "Wireless iAP", onIap);
        listen(CARPLAY_DEVICE, "CarPlay", Bluetooth::closeQuietly);
        findConnectedUnit(BluetoothProfile.HEADSET);
        findConnectedUnit(BluetoothProfile.A2DP);
        if (adapter.isEnabled()) {
            Variant.makeDiscoverable(adapter);
        } else {
            adapter.enable();
        }
    }

    /** Whether we opened this channel rather than the unit. */
    boolean wasDialled(BluetoothSocket socket) {
        return dialled.remove(socket);
    }

    /**
     * A unit already connected when carjack starts, through the phone's own profiles: there is no
     * link-up to learn it from, and it's the one in range.
     */
    private void findConnectedUnit(int profile) {
        adapter.getProfileProxy(context, new BluetoothProfile.ServiceListener() {
            @Override
            public void onServiceConnected(int which, BluetoothProfile proxy) {
                try {
                    for (BluetoothDevice device : proxy.getConnectedDevices()) {
                        if (lastLinked == null && isTarget(device)) {
                            Log.i(TAG, "Head unit " + device.getAddress() + " is connected");
                            setLinked(device);
                        }
                    }
                } catch (SecurityException e) {
                    Log.w(TAG, "Can't list connected devices: " + e.getMessage());
                }
                adapter.closeProfileProxy(which, proxy);
            }

            @Override
            public void onServiceDisconnected(int which) {}
        }, profile);
    }

    /** Paired units hosting iAP: what a user can pick from. */
    java.util.List<BluetoothDevice> pairedUnits() {
        java.util.List<BluetoothDevice> units = new java.util.ArrayList<>();
        if (adapter.isEnabled()) {
            for (BluetoothDevice device : adapter.getBondedDevices()) {
                if (hostsIap(device.getUuids())) {
                    units.add(device);
                }
            }
        }
        return units;
    }

    void setOnUnitLinked(Consumer<BluetoothDevice> listener) {
        onUnitLinked = listener;
    }

    /** Withdraw the services and stop listening, so a later start can register them again. */
    void stop() {
        running = false;
        try {
            context.unregisterReceiver(receiver);
        } catch (IllegalArgumentException ignored) {
        }
        for (BluetoothServerSocket server : servers) {
            try {
                server.close();
            } catch (IOException ignored) {
            }
        }
        servers.clear();
    }

    String name() {
        return adapter.getName();
    }

    String address() {
        return Variant.bluetoothAddress(context, adapter);
    }

    /** Accepts on {@code uuid} until the adapter goes away, then again once it is back. */
    private void listen(UUID uuid, String name, Consumer<BluetoothSocket> onAccept) {
        Thread thread = new Thread(() -> {
            while (running) {
                try (BluetoothServerSocket server =
                        adapter.listenUsingRfcommWithServiceRecord(name, uuid)) {
                    servers.add(server);
                    Log.i(TAG, "Listening for " + name);
                    while (running) {
                        BluetoothSocket socket = server.accept();
                        Log.i(TAG, name + " connection from " + socket.getRemoteDevice());
                        onAccept.accept(socket);
                    }
                } catch (IOException | SecurityException e) {
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
        // Of several paired units, the one whose link came up last is the one that's here.
        BluetoothDevice linked = lastLinked;
        if (linked != null) {
            return linked;
        }
        for (BluetoothDevice device : adapter.getBondedDevices()) {
            if (isTarget(device)) {
                return device;
            }
        }
        return null;
    }

    private String target() {
        String target = Variant.setting(context, "target", "").trim();
        return target.equalsIgnoreCase("auto") ? "" : target;
    }

    private boolean isNamedTarget(BluetoothDevice device) {
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

    static boolean hostsIap(Parcelable[] uuids) {
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

    /**
     * Only where no one is at a screen to pair (the platform build): on a phone the user pairs,
     * and a second bond started alongside theirs fails both.
     */
    private void pair(BluetoothDevice device) {
        if (!Variant.canPage()) {
            return;
        }
        Log.i(TAG, "Found head unit " + device.getAddress() + " (" + device.getName()
                + "); pairing");
        adapter.cancelDiscovery();
        device.createBond();
    }

    /** Drop any link and page again, so the unit offers a fresh iAP channel. */
    void relink(BluetoothDevice device) {
        Log.i(TAG, "Relinking " + device.getAddress());
        if (!Variant.canPage()) {
            Variant.cycleBluetooth();
        } else if (Variant.isConnected(device)) {
            Variant.disconnect(device);
            handler.postDelayed(() -> Variant.connect(device), 1000);
        } else {
            Variant.connect(device);
        }
    }

    /**
     * Open the unit's own iAP service, for where it won't dial us and we can't page it. The
     * channel is handed on as if the unit had dialled.
     */
    void dial(BluetoothDevice device) {
        Thread thread = new Thread(() -> {
            Log.i(TAG, "Dialling " + device.getAddress() + "'s iAP service");
            try {
                adapter.cancelDiscovery();
                BluetoothSocket socket = device.createRfcommSocketToServiceRecord(IAP_ACCESSORY);
                socket.connect();
                Log.i(TAG, "iAP channel dialled to " + device.getAddress());
                dialled.add(socket);
                onIap.accept(socket);
            } catch (IOException | SecurityException e) {
                Log.w(TAG, "Dialling " + device.getAddress() + " failed: " + e.getMessage());
            }
        }, "carjack-dial");
        thread.setDaemon(true);
        thread.start();
    }

    private void setLinked(BluetoothDevice device) {
        lastLinked = device;
        Variant.onLinked(context, device != null);
    }

    /** The unit's link is up, as far as an app can tell. */
    boolean isLinked(BluetoothDevice device) {
        return device.equals(lastLinked);
    }

    void page(BluetoothDevice device) {
        if (!Variant.isConnected(device)) {
            Log.i(TAG, "Paging " + device.getAddress());
            Variant.connect(device);
        }
    }

    private final BroadcastReceiver receiver = new BroadcastReceiver() {
        @Override
        public void onReceive(Context context, Intent intent) {
            BluetoothDevice device =
                    intent.getParcelableExtra(BluetoothDevice.EXTRA_DEVICE, BluetoothDevice.class);
            switch (intent.getAction()) {
                case BluetoothDevice.ACTION_ACL_CONNECTED:
                    // A unit still pairing is left alone: dialling it, or cycling Bluetooth on
                    // it, breaks the pairing.
                    if (isTarget(device)
                            && device.getBondState() == BluetoothDevice.BOND_BONDED) {
                        setLinked(device);
                        onUnitLinked.accept(device);
                    }
                    break;
                case BluetoothDevice.ACTION_BOND_STATE_CHANGED:
                    // Just paired, on a link that came up while pairing.
                    if (intent.getIntExtra(BluetoothDevice.EXTRA_BOND_STATE, -1)
                            == BluetoothDevice.BOND_BONDED && isTarget(device)) {
                        setLinked(device);
                        onUnitLinked.accept(device);
                    }
                    break;
                case BluetoothDevice.ACTION_ACL_DISCONNECTED:
                    if (device.equals(lastLinked)) {
                        setLinked(null);
                    }
                    break;
                case BluetoothAdapter.ACTION_STATE_CHANGED:
                    if (intent.getIntExtra(BluetoothAdapter.EXTRA_STATE, -1)
                            == BluetoothAdapter.STATE_ON) {
                        Variant.makeDiscoverable(adapter);
                    }
                    break;
                case BluetoothDevice.ACTION_PAIRING_REQUEST:
                    if (Variant.confirmPairing(device, intent) && isOrderedBroadcast()) {
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

    static void closeQuietly(BluetoothSocket socket) {
        try {
            socket.close();
        } catch (IOException ignored) {
        }
    }
}
