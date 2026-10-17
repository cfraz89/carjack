package com.carjack;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
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

    /** How often to page an absent unit, and how long to wait before cycling a link that is up. */
    private static final long OFFER_WAIT_MS = 20_000;
    private static final long OFFER_WAIT_MAX_MS = 60_000;

    private final HandlerThread thread = new HandlerThread("carjack");
    private Handler handler;
    private Bluetooth bluetooth;
    private AccessoryWifi wifi;
    private Session session;
    private long offerWait = OFFER_WAIT_MS;
    private long waitingSince;

    @Override
    public void onCreate() {
        super.onCreate();
        Variant.prepare(this);
        NotificationManager nm = getSystemService(NotificationManager.class);
        nm.createNotificationChannel(
                new NotificationChannel(CHANNEL, "CarPlay", NotificationManager.IMPORTANCE_LOW));
        Notification notification = new Notification.Builder(this, CHANNEL)
                .setContentTitle("carjack")
                .setSmallIcon(android.R.drawable.stat_sys_data_bluetooth)
                .build();
        startForeground(1, notification, Variant.foregroundServiceTypes());

        Native.init(new File(getFilesDir(), "homekit.bin").getPath());
        thread.start();
        handler = new Handler(thread.getLooper());
        wifi = new AccessoryWifi(this, handler);
        bluetooth = new Bluetooth(this, handler, socket -> handler.post(() -> accept(socket)));
        handler.post(() -> {
            bluetooth.start();
            waitingSince = now();
            handler.post(watch);
        });
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
            if (!unit.isConnected()) {
                bluetooth.page(unit);
            } else if (Boolean.parseBoolean(Variant.setting(CarjackService.this, "relink", "false"))) {
                Log.i(TAG, "No iAP channel offered in " + offerWait + "ms; cycling the link");
                bluetooth.relink(unit);
                offerWait = Math.min(offerWait * 2, OFFER_WAIT_MAX_MS);
            }
        }
    };

    private void accept(BluetoothSocket socket) {
        if (session != null) {
            Log.w(TAG, "Already in a session; refusing " + socket.getRemoteDevice());
            Bluetooth.closeQuietly(socket);
            return;
        }
        offerWait = OFFER_WAIT_MS;
        BluetoothDevice unit = socket.getRemoteDevice();
        session = new Session(this, handler, socket, wifi, ended -> {
            if (session == ended) {
                session = null;
                waitingSince = now();
                // The unit only dials iAP on a fresh link, and stays connected after a session
                // ends; cycle it so the next one can start.
                bluetooth.relink(unit);
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
        return Variant.onStartCommand(this, intent);
    }

    @Override
    public void onDestroy() {
        Variant.onServiceDestroyed(this);
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }
}
