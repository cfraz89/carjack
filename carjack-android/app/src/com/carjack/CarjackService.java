package com.carjack;

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
 * Waits for a CarPlay unit, pages it until it dials the iAP service, and runs one session at a
 * time. The platform build runs it from boot; the generic one from its activity.
 */
public final class CarjackService extends Service {
    private static final String TAG = "carjack";
    private static final String CHANNEL = "carjack";
    /** The notification's Stop. */
    static final String ACTION_STOP = "com.carjack.STOP";

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

    @Override
    public void onCreate() {
        super.onCreate();
        Variant.prepare(this);
        NotificationManager nm = getSystemService(NotificationManager.class);
        nm.createNotificationChannel(
                new NotificationChannel(CHANNEL, "CarPlay", NotificationManager.IMPORTANCE_LOW));
        PendingIntent stop = PendingIntent.getService(this, 0,
                new Intent(this, CarjackService.class).setAction(ACTION_STOP),
                PendingIntent.FLAG_IMMUTABLE);
        notification = new Notification.Builder(this, CHANNEL)
                .setContentTitle("carjack")
                .setContentText("Waiting for the bike")
                .setSmallIcon(android.R.drawable.stat_sys_data_bluetooth)
                .setOnlyAlertOnce(true)
                .addAction(new Notification.Action.Builder(null, "Stop", stop).build());
        startForeground(1, notification.build(), Variant.foregroundServiceTypes());
        Variant.onStatus(this, "Waiting for the bike");

        Native.init(new File(getFilesDir(), "homekit.bin").getPath());
        thread.start();
        handler = new Handler(thread.getLooper());
        wifi = new AccessoryWifi(this, handler);
        bluetooth = new Bluetooth(this, handler, socket -> handler.post(() -> accept(socket)));
        if (!Variant.canPage()) {
            // A unit dials iAP only on a fresh link, and may carry it on a channel we dial then
            // too: give it a moment to dial first.
            bluetooth.setOnUnitLinked(unit -> {
                // A fresh link: give it the full wait before renewing it.
                waitingSince = now();
                offerWait = OFFER_WAIT_MS;
                if (session == null) {
                    status(name(unit) + " connected; waiting for it to offer CarPlay");
                }
                handler.postDelayed(() -> {
                    if (session == null) {
                        status("Calling " + name(unit) + "'s CarPlay service");
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

    /** Where things are up to, in the notification. */
    void status(String text) {
        Log.i(TAG, "Status: " + text);
        Variant.onStatus(this, text);
        synchronized (notification) {
            getSystemService(NotificationManager.class)
                    .notify(1, notification.setContentText(text).build());
        }
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
                // Nothing to do until the unit links: its link coming up is the cue.
                if (unit != null && bluetooth.isLinked(unit)) {
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
            } else if (Boolean.parseBoolean(Variant.setting(CarjackService.this, "relink", "false"))) {
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
            status("No CarPlay from " + name(unit) + " yet; renewing the Bluetooth link");
        }
        if (cyclesLeft > 0 && Variant.cycleBluetooth()) {
            cyclesLeft--;
        } else {
            status("Calling " + name(unit) + "'s CarPlay service");
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
        status("Setting up CarPlay with " + name(unit) + " over Bluetooth (iAP2)");
        session = new Session(this, handler, socket, wifi, this::status, ended -> {
            if (session == ended) {
                session = null;
                Variant.onSessionEnded(this, unit, ended.error);
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
                    status("CarPlay keeps failing with " + name(unit)
                            + "; waiting for it to reconnect");
                }
            }
        });
        try {
            session.start(bluetooth.name(), Variant.addressFor(this, bluetooth.address(), unit),
                    codec());
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
            if (session != null) {
                session.stop();
            }
            bluetooth.stop();
            wifi.leave();
            thread.quitSafely();
        });
        Variant.onServiceDestroyed(this);
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }
}
