package io.github.cfraz89.slipstream

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.bluetooth.BluetoothDevice
import android.bluetooth.BluetoothSocket
import android.content.Context
import android.content.Intent
import android.os.Handler
import android.os.HandlerThread
import android.os.IBinder
import android.os.SystemClock
import android.util.Log
import java.io.File
import java.io.IOException

/**
 * Waits for a head unit, pages it until it dials the iAP service, and runs one session at a
 * time. The platform build runs it from boot; the generic one from its activity.
 */
class SlipstreamService : Service() {
    companion object {
        private const val TAG = "carjack"
        const val CHANNEL = "carjack"
        const val NOTIFICATION_ID = 1
        /** The notification's Turn off. */
        const val ACTION_STOP = "io.github.cfraz89.slipstream.STOP"
        /** Try again after giving up on a unit. */
        const val ACTION_RETRY = "io.github.cfraz89.slipstream.RETRY"
        const val WAITING = "Waiting for the head unit"

        /** How often to page an absent unit, and how long to wait before cycling a link that is up. */
        private const val OFFER_WAIT_MS = 20_000L
        private const val OFFER_WAIT_MAX_MS = 60_000L
        /** Once the head unit's link is up, how long it gets to dial iAP before we dial it. */
        private const val UNIT_DIAL_GRACE_MS = 8_000L
        /** How long a session on a channel we dialled is still setting up iAP2, and can give way. */
        private const val DIALLED_SETUP_MS = 6_000L
        private const val MAX_CYCLES = 2
        private const val MAX_FAILURES = 3

        fun createChannel(context: Context) {
            context.getSystemService(NotificationManager::class.java).createNotificationChannel(
                NotificationChannel(CHANNEL, "Connection", NotificationManager.IMPORTANCE_LOW),
            )
        }

        private fun name(device: BluetoothDevice): String = try {
            device.name ?: device.address
        } catch (e: SecurityException) {
            device.address
        }

        private fun now() = SystemClock.elapsedRealtime()
    }

    private val thread = HandlerThread("carjack")
    private lateinit var notification: Notification.Builder
    private lateinit var handler: Handler
    private lateinit var bluetooth: Bluetooth
    private lateinit var wifi: AccessoryWifi
    private var session: Session? = null
    private var sessionDialled = false
    private var sessionStarted = 0L
    /** Where we can't page, a unit already linked at start gets only the dial grace. */
    private var offerWait = if (Variant.canPage()) OFFER_WAIT_MS else UNIT_DIAL_GRACE_MS
    private var waitingSince = 0L
    /** Bluetooth cycles left before only dialling; refilled by a session. */
    private var cyclesLeft = MAX_CYCLES
    /** Sessions in a row that never got to video. */
    private var failures = 0
    /** The head unit given up on, for Retry. */
    private var gaveUpOn: BluetoothDevice? = null
    private lateinit var stopAction: Notification.Action
    private lateinit var retryAction: Notification.Action

    override fun onCreate() {
        super.onCreate()
        Variant.prepare(this)
        createChannel(this)
        stopAction = action("Turn off", ACTION_STOP, 0)
        retryAction = action("Retry", ACTION_RETRY, 1)
        notification = Notification.Builder(this, CHANNEL)
            .setContentTitle(applicationInfo.loadLabel(packageManager))
            .setContentText(WAITING)
            .setSmallIcon(Variant.NOTIFICATION_ICON)
            .setOnlyAlertOnce(true)
            .setOngoing(true)
            .setContentIntent(Variant.openApp(this))
            .setActions(stopAction)
        startForeground(NOTIFICATION_ID, notification.build(), Variant.foregroundServiceTypes())
        Variant.onStatus(this, WAITING, false)

        Native.init(File(filesDir, "homekit.bin").path, Variant.REPORTER)
        thread.start()
        handler = Handler(thread.looper)
        wifi = AccessoryWifi(this, handler)
        AccessoryWifi.reset()
        bluetooth = Bluetooth(this, handler) { socket -> handler.post { accept(socket) } }
        if (!Variant.canPage()) {
            // A unit dials iAP only on a fresh link, and may carry it on a channel we dial then
            // too: give it a moment to dial first.
            bluetooth.setOnUnitLinked { unit ->
                // A fresh link: give it the full wait before renewing it.
                gaveUpOn = null
                waitingSince = now()
                offerWait = OFFER_WAIT_MS
                if (session == null) status("${name(unit)} connected")
                handler.postDelayed({
                    if (session == null) {
                        status("Calling ${name(unit)}")
                        bluetooth.dial(unit)
                    }
                }, UNIT_DIAL_GRACE_MS)
            }
        }
        handler.post {
            bluetooth.start()
            waitingSince = now()
            handler.post(watch)
        }
    }

    private fun action(title: String, intentAction: String, requestCode: Int): Notification.Action {
        val intent = PendingIntent.getService(
            this, requestCode, Intent(this, SlipstreamService::class.java).setAction(intentAction),
            PendingIntent.FLAG_IMMUTABLE,
        )
        return Notification.Action.Builder(null, title, intent).build()
    }

    /** Where things are up to, in the notification; [retryable] offers Retry where it has given up. */
    fun status(text: String, retryable: Boolean = false) {
        Log.i(TAG, "Status: $text")
        Variant.onStatus(this, text, retryable)
        synchronized(notification) {
            notification.setContentText(text)
            if (retryable) {
                notification.setActions(retryAction, stopAction)
            } else {
                notification.setActions(stopAction)
            }
            getSystemService(NotificationManager::class.java)
                .notify(NOTIFICATION_ID, notification.build())
        }
    }

    /** A fresh start with the head unit given up on: the failure count cleared, its link renewed. */
    private fun retry() {
        failures = 0
        cyclesLeft = MAX_CYCLES
        val unit = gaveUpOn ?: bluetooth.headUnit()
        gaveUpOn = null
        if (session != null || unit == null) {
            status(if (session != null) "Already in a session" else WAITING)
            return
        }
        status("Trying ${name(unit)} again")
        waitingSince = now()
        bluetooth.relink(unit)
    }

    /** Without a session: find the head unit, and page it until it dials in. */
    private val watch = object : Runnable {
        override fun run() {
            handler.postDelayed(this, 2000)
            if (session != null) return
            val unit = bluetooth.headUnit()
            if (!Variant.canPage()) {
                // Nothing to do until the head unit links: its link coming up is the cue. A unit given
                // up on is left alone until it relinks or the user retries.
                if (unit != null && bluetooth.isLinked(unit) && gaveUpOn == null) renewLink(unit)
                return
            }
            if (unit == null) {
                if (now() - waitingSince >= Bluetooth.DISCOVERY_INTERVAL_MS) {
                    bluetooth.discover()
                    waitingSince = now()
                }
                return
            }
            // Android's stack brings its own profiles up on a page, which is the head unit's cue to dial
            // iAP; the link is only cycled if that turns out not to be enough.
            if (now() - waitingSince < offerWait) return
            waitingSince = now()
            if (!Variant.isConnected(unit)) {
                bluetooth.page(unit)
            } else if (Variant.setting(this@SlipstreamService, "relink", "false").toBoolean()) {
                Log.i(TAG, "No iAP channel offered in ${offerWait}ms; cycling the link")
                bluetooth.relink(unit)
                offerWait = minOf(offerWait * 2, OFFER_WAIT_MAX_MS)
            }
        }
    }

    /**
     * Where we can't page: a linked unit that hasn't dialled iAP gets a renewed link, a couple of
     * times at most so a unit that won't do CarPlay doesn't have Bluetooth cycling all day, and
     * after that is only dialled.
     */
    private fun renewLink(unit: BluetoothDevice) {
        if (now() - waitingSince < offerWait) return
        waitingSince = now()
        if (cyclesLeft > 0) status("Renewing the link to ${name(unit)}")
        if (cyclesLeft > 0 && Variant.cycleBluetooth()) {
            cyclesLeft--
            status(WAITING)
        } else {
            status("Calling ${name(unit)}")
            bluetooth.dial(unit)
        }
        offerWait = minOf(offerWait * 2, OFFER_WAIT_MAX_MS)
    }

    private fun accept(socket: BluetoothSocket) {
        val dialled = bluetooth.wasDialled(socket)
        session?.let { current ->
            // A unit carries iAP2 on the channel it opened over one we dialled, if it opens one.
            if (sessionDialled && !dialled && now() - sessionStarted < DIALLED_SETUP_MS) {
                Log.i(TAG, "The head unit dialled in; dropping the channel we dialled")
                session = null
                current.stop()
            } else {
                Log.w(TAG, "Already in a session; refusing ${socket.remoteDevice}")
                Bluetooth.closeQuietly(socket)
                return
            }
        }
        sessionDialled = dialled
        sessionStarted = now()
        offerWait = OFFER_WAIT_MS
        val unit = socket.remoteDevice
        status("Connecting to ${name(unit)}")
        val started = Session(this, handler, socket, wifi, { status(it) }) { ended ->
            if (session !== ended) return@Session
            session = null
            waitingSince = now()
            if (ended.streamed) {
                failures = 0
                cyclesLeft = MAX_CYCLES
            } else {
                failures++
            }
            // The head unit only dials iAP on a fresh link, and stays connected after a session
            // ends; cycle it so the next one can start - unless they keep failing, when it
            // waits for the head unit to reconnect by itself.
            if (failures < MAX_FAILURES) {
                bluetooth.relink(unit)
            } else {
                gaveUpOn = unit
                status("Sessions keep failing with ${name(unit)}", true)
            }
        }
        session = started
        try {
            started.start(bluetooth.name(), bluetooth.address(), codec())
        } catch (e: IOException) {
            Log.e(TAG, "Could not start a session", e)
            Bluetooth.closeQuietly(socket)
            session = null
        }
    }

    private fun codec() = when (Variant.setting(this, "codec", "auto")) {
        "h264" -> Native.CODEC_H264
        "hevc" -> Native.CODEC_HEVC
        else -> Native.CODEC_AUTO
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> {
                Variant.onStopRequested(this)
                stopSelf()
                return START_NOT_STICKY
            }
            ACTION_RETRY -> handler.post(::retry)
        }
        return Variant.onStartCommand(this, intent)
    }

    /** Re-declare the foreground service with more types, once their consent is in hand. */
    fun promote(foregroundServiceTypes: Int) {
        startForeground(NOTIFICATION_ID, notification.build(), foregroundServiceTypes)
    }

    override fun onDestroy() {
        handler.removeCallbacks(watch)
        handler.post {
            // Cleared first, so the session's end isn't taken for a dropped one and relinked.
            val stopping = session
            session = null
            stopping?.stop()
            bluetooth.stop()
            wifi.leave()
            // After leave(), so the variant's own teardown follows the WiFi being handed back.
            Variant.onServiceDestroyed(this)
            thread.quitSafely()
        }
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null
}
