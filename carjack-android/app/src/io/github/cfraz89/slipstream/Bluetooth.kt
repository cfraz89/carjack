package io.github.cfraz89.slipstream

import android.bluetooth.BluetoothAdapter
import android.bluetooth.BluetoothDevice
import android.bluetooth.BluetoothManager
import android.bluetooth.BluetoothProfile
import android.bluetooth.BluetoothServerSocket
import android.bluetooth.BluetoothSocket
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.Handler
import android.os.ParcelUuid
import android.os.Parcelable
import android.os.SystemClock
import android.util.Log
import java.io.IOException
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap

/**
 * The Bluetooth side a phone shows a CarPlay unit: discoverable, the iAP service the head unit dials in
 * on, and paging the head unit to get it to dial. How much of that happens without asking is up to the
 * [Variant].
 */
class Bluetooth(
    private val context: Context,
    private val handler: Handler,
    private val onIap: (BluetoothSocket) -> Unit,
) {
    companion object {
        private const val TAG = "carjack"

        val IAP_DEVICE: UUID = UUID.fromString("00000000-deca-fade-deca-deafdecacafe")
        val IAP_ACCESSORY: UUID = UUID.fromString("00000000-deca-fade-deca-deafdecacaff")
        /** Only advertised: units look for it in EIR while bonding. */
        val CARPLAY_DEVICE: UUID = UUID.fromString("2d8d2466-e14d-451c-88bc-7301abea291a")

        private const val RELISTEN_DELAY_MS = 2000L
        const val DISCOVERY_INTERVAL_MS = 15000L
        private const val PROBE_WAIT_MS = 15000L
        private const val PROBE_RETRY_MS = 60000L

        fun hostsIap(uuids: Array<out Parcelable>?): Boolean =
            uuids?.any { it is ParcelUuid && it.uuid == IAP_ACCESSORY } == true

        fun closeQuietly(socket: BluetoothSocket) {
            try {
                socket.close()
            } catch (ignored: IOException) {
            }
        }
    }

    private val adapter: BluetoothAdapter =
        context.getSystemService(BluetoothManager::class.java).adapter
    @Volatile private var lastLinked: BluetoothDevice? = null
    /** The head unit's link has just come up: when it dials iAP, if it is going to. */
    private var onUnitLinked: (BluetoothDevice) -> Unit = {}

    /** When each device was last asked for its services. */
    private val probed = HashMap<String, Long>()
    /** Discovery holds off until then, while a probe is out. */
    private var probeUntil = 0L
    @Volatile private var running = false
    private val dialled = ConcurrentHashMap.newKeySet<BluetoothSocket>()
    /** Open listening sockets, closed to stop their threads. */
    private val servers = ConcurrentHashMap.newKeySet<BluetoothServerSocket>()

    fun start() {
        running = true
        setLinked(null)
        val filter = IntentFilter().apply {
            addAction(BluetoothDevice.ACTION_PAIRING_REQUEST)
            addAction(BluetoothDevice.ACTION_FOUND)
            addAction(BluetoothDevice.ACTION_UUID)
            addAction(BluetoothAdapter.ACTION_STATE_CHANGED)
            addAction(BluetoothDevice.ACTION_ACL_CONNECTED)
            addAction(BluetoothDevice.ACTION_ACL_DISCONNECTED)
            addAction(BluetoothDevice.ACTION_BOND_STATE_CHANGED)
            priority = IntentFilter.SYSTEM_HIGH_PRIORITY
        }
        context.registerReceiver(receiver, filter, null, handler, Context.RECEIVER_EXPORTED)
        // The listeners retry until the adapter is on, which a user may still have to do.
        listen(IAP_DEVICE, "Wireless iAP", onIap)
        listen(CARPLAY_DEVICE, "CarPlay", ::closeQuietly)
        findConnectedUnit(BluetoothProfile.HEADSET)
        findConnectedUnit(BluetoothProfile.A2DP)
        @Suppress("DEPRECATION")
        if (adapter.isEnabled) Variant.makeDiscoverable(adapter) else adapter.enable()
    }

    /** Whether we opened this channel rather than the head unit. */
    fun wasDialled(socket: BluetoothSocket) = dialled.remove(socket)

    /**
     * A unit already connected when carjack starts, through the phone's own profiles: there is no
     * link-up to learn it from, and it's the one in range.
     */
    private fun findConnectedUnit(profile: Int) {
        adapter.getProfileProxy(context, object : BluetoothProfile.ServiceListener {
            override fun onServiceConnected(which: Int, proxy: BluetoothProfile) {
                try {
                    for (device in proxy.connectedDevices) {
                        if (lastLinked == null && isTarget(device)) {
                            Log.i(TAG, "Head unit ${device.address} is connected")
                            setLinked(device)
                        }
                    }
                } catch (e: SecurityException) {
                    Log.w(TAG, "Can't list connected devices: ${e.message}")
                }
                adapter.closeProfileProxy(which, proxy)
            }

            override fun onServiceDisconnected(which: Int) {}
        }, profile)
    }

    fun setOnUnitLinked(listener: (BluetoothDevice) -> Unit) {
        onUnitLinked = listener
    }

    /** Withdraw the services and stop listening, so a later start can register them again. */
    fun stop() {
        running = false
        try {
            context.unregisterReceiver(receiver)
        } catch (ignored: IllegalArgumentException) {
        }
        for (server in servers) {
            try {
                server.close()
            } catch (ignored: IOException) {
            }
        }
        servers.clear()
    }

    fun name(): String? = adapter.name

    fun address(): String? = Variant.bluetoothAddress(context, adapter)

    /** Accepts on [uuid] until the adapter goes away, then again once it is back. */
    private fun listen(uuid: UUID, name: String, onAccept: (BluetoothSocket) -> Unit) {
        val thread = Thread({
            while (running) {
                try {
                    adapter.listenUsingRfcommWithServiceRecord(name, uuid).use { server ->
                        servers.add(server)
                        Log.i(TAG, "Listening for $name")
                        while (running) {
                            val socket = server.accept()
                            Log.i(TAG, "$name connection from ${socket.remoteDevice}")
                            onAccept(socket)
                        }
                    }
                } catch (e: IOException) {
                    Log.w(TAG, "$name listener ended: ${e.message}")
                } catch (e: SecurityException) {
                    Log.w(TAG, "$name listener ended: ${e.message}")
                }
                try {
                    Thread.sleep(RELISTEN_DELAY_MS)
                } catch (e: InterruptedException) {
                    return@Thread
                }
            }
        }, "carjack-$name")
        thread.isDaemon = true
        thread.start()
    }

    /** The configured unit, or else any bonded one that hosts iAP. */
    fun headUnit(): BluetoothDevice? {
        if (!adapter.isEnabled) return null
        // Of several paired units, the one whose link came up last is the one that's here.
        lastLinked?.let { return it }
        return adapter.bondedDevices.firstOrNull { isTarget(it) }
    }

    private fun target(): String {
        val target = Variant.setting(context, "target", "").trim()
        return if (target.equals("auto", ignoreCase = true)) "" else target
    }

    private fun isNamedTarget(device: BluetoothDevice): Boolean {
        val target = target()
        return target.isNotEmpty() &&
            (target.equals(device.address, ignoreCase = true) || target == device.name)
    }

    private fun isTarget(device: BluetoothDevice): Boolean {
        val target = target()
        if (target.isNotEmpty()) {
            return target.equals(device.address, ignoreCase = true) || target == device.name
        }
        return hostsIap(device.uuids)
    }

    /** Look for an unbonded unit to pair with. */
    fun discover() {
        if (adapter.isEnabled && !adapter.isDiscovering &&
            SystemClock.elapsedRealtime() >= probeUntil
        ) {
            adapter.startDiscovery()
        }
    }

    /**
     * Ask one device for its services. The stack defers SDP behind a running inquiry and drops it
     * when the next one starts, so discovery stops for the duration.
     */
    private fun probe(device: BluetoothDevice) {
        val now = SystemClock.elapsedRealtime()
        val last = probed[device.address]
        if (now < probeUntil || (last != null && now - last < PROBE_RETRY_MS)) return
        probed[device.address] = now
        probeUntil = now + PROBE_WAIT_MS
        Log.i(TAG, "Asking ${device.address} (${device.name}) for its services")
        adapter.cancelDiscovery()
        device.fetchUuidsWithSdp()
    }

    /**
     * Only where no one is at a screen to pair (the platform build): on a phone the user pairs,
     * and a second bond started alongside theirs fails both.
     */
    private fun pair(device: BluetoothDevice) {
        if (!Variant.canPage()) return
        Log.i(TAG, "Found head unit ${device.address} (${device.name}); pairing")
        adapter.cancelDiscovery()
        device.createBond()
    }

    /** Drop any link and page again, so the head unit offers a fresh iAP channel. */
    fun relink(device: BluetoothDevice) {
        Log.i(TAG, "Relinking ${device.address}")
        when {
            !Variant.canPage() -> Variant.cycleBluetooth()
            Variant.isConnected(device) -> {
                Variant.disconnect(device)
                handler.postDelayed({ Variant.connect(device) }, 1000)
            }
            else -> Variant.connect(device)
        }
    }

    /**
     * Open the head unit's own iAP service, for where it won't dial us and we can't page it. The
     * channel is handed on as if the head unit had dialled.
     */
    fun dial(device: BluetoothDevice) {
        val thread = Thread({
            Log.i(TAG, "Dialling ${device.address}'s iAP service")
            try {
                adapter.cancelDiscovery()
                val socket = device.createRfcommSocketToServiceRecord(IAP_ACCESSORY)
                socket.connect()
                Log.i(TAG, "iAP channel dialled to ${device.address}")
                dialled.add(socket)
                onIap(socket)
            } catch (e: IOException) {
                Log.w(TAG, "Dialling ${device.address} failed: ${e.message}")
            } catch (e: SecurityException) {
                Log.w(TAG, "Dialling ${device.address} failed: ${e.message}")
            }
        }, "carjack-dial")
        thread.isDaemon = true
        thread.start()
    }

    private fun setLinked(device: BluetoothDevice?) {
        lastLinked = device
        Variant.onLinked(context, device != null)
    }

    /** The head unit's link is up, as far as an app can tell. */
    fun isLinked(device: BluetoothDevice) = device == lastLinked

    fun page(device: BluetoothDevice) {
        if (!Variant.isConnected(device)) {
            Log.i(TAG, "Paging ${device.address}")
            Variant.connect(device)
        }
    }

    private val receiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            val device = intent.getParcelableExtra(
                BluetoothDevice.EXTRA_DEVICE, BluetoothDevice::class.java,
            )
            when (intent.action) {
                // A unit still pairing is left alone: dialling it, or cycling Bluetooth on it,
                // breaks the pairing.
                BluetoothDevice.ACTION_ACL_CONNECTED -> if (device != null && isTarget(device) &&
                    device.bondState == BluetoothDevice.BOND_BONDED
                ) {
                    setLinked(device)
                    onUnitLinked(device)
                }
                // Just paired, on a link that came up while pairing.
                BluetoothDevice.ACTION_BOND_STATE_CHANGED -> if (device != null &&
                    intent.getIntExtra(BluetoothDevice.EXTRA_BOND_STATE, -1) ==
                    BluetoothDevice.BOND_BONDED && isTarget(device)
                ) {
                    setLinked(device)
                    onUnitLinked(device)
                }
                BluetoothDevice.ACTION_ACL_DISCONNECTED ->
                    if (device != null && device == lastLinked) setLinked(null)
                BluetoothAdapter.ACTION_STATE_CHANGED ->
                    if (intent.getIntExtra(BluetoothAdapter.EXTRA_STATE, -1) ==
                        BluetoothAdapter.STATE_ON
                    ) {
                        Variant.makeDiscoverable(adapter)
                    }
                BluetoothDevice.ACTION_PAIRING_REQUEST ->
                    if (device != null && Variant.confirmPairing(device, intent) &&
                        isOrderedBroadcast
                    ) {
                        abortBroadcast()
                    }
                BluetoothDevice.ACTION_FOUND -> {
                    if (device == null || device.bondState != BluetoothDevice.BOND_NONE ||
                        device.type == BluetoothDevice.DEVICE_TYPE_LE
                    ) {
                        return
                    }
                    if (isNamedTarget(device)) {
                        pair(device)
                    } else if (target().isEmpty()) {
                        probe(device)
                    }
                }
                BluetoothDevice.ACTION_UUID -> {
                    if (device == null) return
                    val uuids = intent.getParcelableArrayExtra(BluetoothDevice.EXTRA_UUID)
                    Log.i(
                        TAG,
                        "Services of ${device.address}: " +
                            (uuids?.contentToString() ?: "none"),
                    )
                    probeUntil = 0
                    if (device.bondState == BluetoothDevice.BOND_NONE && hostsIap(uuids)) {
                        pair(device)
                    }
                }
            }
        }
    }
}
