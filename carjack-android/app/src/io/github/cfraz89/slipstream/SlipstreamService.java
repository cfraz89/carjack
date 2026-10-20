package io.github.cfraz89.slipstream;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.bluetooth.BluetoothDevice;
import android.bluetooth.BluetoothSocket;
import android.content.Intent;
import android.os.Handler;
import android.os.HandlerThread;
import android.os.IBinder;
import android.util.Log;

import java.io.File;
import java.io.IOException;

/**
 * Waits for a head unit, pages it until it dials the iAP service, and runs one session at a
 * time. The platform build runs it from boot; the generic one from its activity.
 */
public final class SlipstreamService extends Service {
    private static final String TAG = "carjack";
    private static final String CHANNEL = "carjack";
    /** The notification's Stop. */
    static final String ACTION_STOP = "io.github.cfraz89.slipstream.STOP";
    /** Try again after giving up on a unit. */
    static final String ACTION_RETRY = "io.github.cfraz89.slipstream.RETRY";

    /** How often to page an absent unit, and how long to wait before cycling a link that is up. */
    private static final long OFFER_WAIT_MS = 20_000;
    private static final long OFFER_WAIT_MAX_MS = 60_000;
    /** Once the unit's link is up, how long it gets to dial iAP before we dial it. */
    private static final long UNIT_DIAL_GRACE_MS = 8_000;
    /** How long a session on a channel we dialled is still setting up iAP2, and can give way. */
    private static final long DIALLED_SETUP_MS = 6_000;

    private final HandlerThread thread = new HandlerThread("carjack");
    private Notification.Builder notification;
    private Handler handler;
    private Bluetooth bluetooth;
    private AccessoryWifi wifi;
    private Session session;
    private boolean sessionDialled;
    private long sessionStarted;
    /** Where we can't page, a unit already linked at start gets only the dial grace. */
    private long offerWait = Variant.canPage() ? OFFER_WAIT_MS : UNIT_DIAL_GRACE_MS;
    private long waitingSince;
    private static final int MAX_CYCLES = 2;
    /** Bluetooth cycles left before only dialling; refilled by a session. */
    private int cyclesLeft = MAX_CYCLES;
    private static final int MAX_FAILURES = 3;
    /** Sessions in a row that never got to video. */
    private int failures;
    /** The unit given up on, for Retry. */
    private BluetoothDevice gaveUpOn;
    private Notification.Action stopAction;
    private Notification.Action retryAction;

    @Override
    public void onCreate() {
        super.onCreate();
        Variant.prepare(this);
        NotificationManager nm = getSystemService(NotificationManager.class);
        nm.createNotificationChannel(
                new NotificationChannel(CHANNEL, "Connection", NotificationManager.IMPORTANCE_LOW));
        stopAction = action("Stop", ACTION_STOP, 0);
        retryAction = action("Retry", ACTION_RETRY, 1);
        notification = new Notification.Builder(this, CHANNEL)
                .setContentTitle(getApplicationInfo().loadLabel(getPackageManager()))
                .setContentText("Waiting for the unit")
                .setSmallIcon(Variant.NOTIFICATION_ICON)
                .setOnlyAlertOnce(true)
                .setActions(stopAction);
        startForeground(1, notification.build(), Variant.foregroundServiceTypes());
        Variant.onStatus(this, "Waiting for the unit", false);

        Native.init(new File(getFilesDir(), "homekit.bin").getPath());
        thread.start();
        handler = new Handler(thread.getLooper());
        wifi = new AccessoryWifi(this, handler);
        AccessoryWifi.reset();
        bluetooth = new Bluetooth(this, handler, socket -> handler.post(() -> accept(socket)));
        if (!Variant.canPage()) {
            // A unit dials iAP only on a fresh link, and may carry it on a channel we dial then
            // too: give it a moment to dial first.
            bluetooth.setOnUnitLinked(unit -> {
                // A fresh link: give it the full wait before renewing it.
                gaveUpOn = null;
                waitingSince = now();
                offerWait = OFFER_WAIT_MS;
                if (session == null) {
                    status(name(unit) + " connected; waiting for it to start a session");
                }
                handler.postDelayed(() -> {
                    if (session == null) {
                        status("Calling " + name(unit));
                        bluetooth.dial(unit);
                    }
                }, UNIT_DIAL_GRACE_MS);
            });
        }
        handler.post(() -> {
            bluetooth.start();
            waitingSince = now();
            handler.post(watch);
        });
    }

    private Notification.Action action(String title, String intentAction, int requestCode) {
        PendingIntent intent = PendingIntent.getService(this, requestCode,
                new Intent(this, SlipstreamService.class).setAction(intentAction),
                PendingIntent.FLAG_IMMUTABLE);
        return new Notification.Action.Builder(null, title, intent).build();
    }

    /** Where things are up to, in the notification. */
    void status(String text) {
        status(text, false);
    }

    /** As {@link #status(String)}, offering Retry where it has given up. */
    private void status(String text, boolean retryable) {
        Log.i(TAG, "Status: " + text);
        Variant.onStatus(this, text, retryable);
        synchronized (notification) {
            notification.setContentText(text);
            if (retryable) {
                notification.setActions(retryAction, stopAction);
            } else {
                notification.setActions(stopAction);
            }
            getSystemService(NotificationManager.class).notify(1, notification.build());
        }
    }

    /** A fresh start with the unit given up on: the failure count cleared, its link renewed. */
    private void retry() {
        failures = 0;
        cyclesLeft = MAX_CYCLES;
        BluetoothDevice unit = gaveUpOn != null ? gaveUpOn : bluetooth.headUnit();
        gaveUpOn = null;
        if (session != null || unit == null) {
            status(session != null ? "Already in a session" : "Waiting for the unit");
            return;
        }
        status("Trying " + name(unit) + " again");
        waitingSince = now();
        bluetooth.relink(unit);
    }

    private static String name(BluetoothDevice device) {
        try {
            String name = device.getName();
            return name != null ? name : device.getAddress();
        } catch (SecurityException e) {
            return device.getAddress();
        }
    }

    private static long now() {
        return android.os.SystemClock.elapsedRealtime();
    }

    /** Without a session: find the unit, and page it until it dials in. */
    private final Runnable watch = new Runnable() {
        @Override
        public void run() {
            handler.postDelayed(this, 2000);
            if (session != null) {
                return;
            }
            BluetoothDevice unit = bluetooth.headUnit();
            if (!Variant.canPage()) {
                // Nothing to do until the unit links: its link coming up is the cue. A unit given
                // up on is left alone until it relinks or the user retries.
                if (unit != null && bluetooth.isLinked(unit) && gaveUpOn == null) {
                    renewLink(unit);
                }
                return;
            }
            if (unit == null) {
                if (now() - waitingSince >= Bluetooth.DISCOVERY_INTERVAL_MS) {
                    bluetooth.discover();
                    waitingSince = now();
                }
                return;
            }
            // Android's stack brings its own profiles up on a page, which is the unit's cue to dial
            // iAP; the link is only cycled if that turns out not to be enough.
            if (now() - waitingSince < offerWait) {
                return;
            }
            waitingSince = now();
            if (!Variant.isConnected(unit)) {
                bluetooth.page(unit);
            } else if (Boolean.parseBoolean(Variant.setting(SlipstreamService.this, "relink", "false"))) {
                Log.i(TAG, "No iAP channel offered in " + offerWait + "ms; cycling the link");
                bluetooth.relink(unit);
                offerWait = Math.min(offerWait * 2, OFFER_WAIT_MAX_MS);
            }
        }
    };

    /**
     * Where we can't page: a linked unit that hasn't dialled iAP gets a renewed link, a couple of
     * times at most so a unit that won't do CarPlay doesn't have Bluetooth cycling all day, and
     * after that is only dialled.
     */
    private void renewLink(BluetoothDevice unit) {
        if (now() - waitingSince < offerWait) {
            return;
        }
        waitingSince = now();
        if (cyclesLeft > 0) {
            status("Nothing from " + name(unit) + " yet; renewing the Bluetooth link");
        }
        if (cyclesLeft > 0 && Variant.cycleBluetooth()) {
            cyclesLeft--;
        } else {
            status("Calling " + name(unit));
            bluetooth.dial(unit);
        }
        offerWait = Math.min(offerWait * 2, OFFER_WAIT_MAX_MS);
    }

    private void accept(BluetoothSocket socket) {
        boolean dialled = bluetooth.wasDialled(socket);
        if (session != null) {
            // A unit carries iAP2 on the channel it opened over one we dialled, if it opens one.
            if (sessionDialled && !dialled && now() - sessionStarted < DIALLED_SETUP_MS) {
                Log.i(TAG, "The unit dialled in; dropping the channel we dialled");
                Session dropped = session;
                session = null;
                dropped.stop();
            } else {
                Log.w(TAG, "Already in a session; refusing " + socket.getRemoteDevice());
                Bluetooth.closeQuietly(socket);
                return;
            }
        }
        sessionDialled = dialled;
        sessionStarted = now();
        offerWait = OFFER_WAIT_MS;
        BluetoothDevice unit = socket.getRemoteDevice();
        status("Setting up a session with " + name(unit) + " over Bluetooth");
        session = new Session(this, handler, socket, wifi, this::status, ended -> {
            if (session == ended) {
                session = null;
                waitingSince = now();
                if (ended.streamed) {
                    failures = 0;
                    cyclesLeft = MAX_CYCLES;
                } else {
                    failures++;
                }
                // The unit only dials iAP on a fresh link, and stays connected after a session
                // ends; cycle it so the next one can start - unless they keep failing, when it
                // waits for the unit to reconnect by itself.
                if (failures < MAX_FAILURES) {
                    bluetooth.relink(unit);
                } else {
                    gaveUpOn = unit;
                    status("Sessions keep failing with " + name(unit)
                            + "; waiting for it to reconnect", true);
                }
            }
        });
        try {
            session.start(bluetooth.name(), bluetooth.address(), codec());
        } catch (IOException e) {
            Log.e(TAG, "Could not start a session", e);
            Bluetooth.closeQuietly(socket);
            session = null;
        }
    }

    private int codec() {
        switch (Variant.setting(this, "codec", "auto")) {
            case "h264":
                return Native.CODEC_H264;
            case "hevc":
                return Native.CODEC_HEVC;
            default:
                return Native.CODEC_AUTO;
        }
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        if (intent != null && ACTION_STOP.equals(intent.getAction())) {
            Variant.onStopRequested(this);
            stopSelf();
            return START_NOT_STICKY;
        }
        if (intent != null && ACTION_RETRY.equals(intent.getAction())) {
            handler.post(this::retry);
        }
        return Variant.onStartCommand(this, intent);
    }

    /** Re-declare the foreground service with more types, once their consent is in hand. */
    void promote(int foregroundServiceTypes) {
        startForeground(1, notification.build(), foregroundServiceTypes);
    }

    @Override
    public void onDestroy() {
        handler.removeCallbacks(watch);
        handler.post(() -> {
            // Cleared first, so the session's end isn't taken for a dropped one and relinked.
            Session stopping = session;
            session = null;
            if (stopping != null) {
                stopping.stop();
            }
            bluetooth.stop();
            wifi.leave();
            // After leave(), so the variant's own teardown follows the WiFi being handed back.
            Variant.onServiceDestroyed(this);
            thread.quitSafely();
        });
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }
}
