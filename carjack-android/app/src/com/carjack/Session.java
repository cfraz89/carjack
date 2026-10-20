package com.carjack;

import android.bluetooth.BluetoothSocket;
import android.content.Context;
import android.os.Handler;
import android.os.ParcelFileDescriptor;
import android.util.Log;

import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;

/** One CarPlay connection: the core's callbacks, answered with the platform's pieces. */
final class Session implements Native.Callbacks {
    private static final String TAG = "carjack";

    interface Listener {
        void ended(Session session);
    }

    private final Context context;
    private final Handler handler;
    private final BluetoothSocket socket;
    private final AccessoryWifi wifi;
    private final Listener listener;
    /** Where the session is up to, for the notification. */
    private final java.util.function.Consumer<String> status;
    private volatile long handle;
    private ScreenStream stream;
    private volatile Touch touch;
    /** Video got as far as starting. */
    volatile boolean streamed;

    Session(Context context, Handler handler, BluetoothSocket socket, AccessoryWifi wifi,
            java.util.function.Consumer<String> status, Listener listener) {
        this.context = context;
        this.handler = handler;
        this.socket = socket;
        this.wifi = wifi;
        this.status = status;
        this.listener = listener;
    }

    /** Relays the RFCOMM channel through a socket pair, since the core wants a plain fd. */
    void start(String name, String address, int codec) throws IOException {
        ParcelFileDescriptor[] pair = ParcelFileDescriptor.createSocketPair();
        ParcelFileDescriptor ours = pair[0];
        pump("carjack-iap-rx", socket.getInputStream(), new FileOutputStream(ours.getFileDescriptor()), ours);
        pump("carjack-iap-tx", new FileInputStream(ours.getFileDescriptor()), socket.getOutputStream(), ours);
        handle = Native.startSession(this, pair[1].detachFd(), name, address, codec);
    }

    private void pump(String name, InputStream in, OutputStream out, ParcelFileDescriptor ours) {
        Thread thread = new Thread(() -> {
            byte[] buf = new byte[4096];
            try {
                int n;
                while ((n = in.read(buf)) > 0) {
                    out.write(buf, 0, n);
                }
            } catch (IOException ignored) {
            }
            Bluetooth.closeQuietly(socket);
            try {
                ours.close();
            } catch (IOException ignored) {
            }
        }, name);
        thread.setDaemon(true);
        thread.start();
    }

    void stop() {
        Native.stopSession(handle);
    }

    @Override
    public void joinWifi(String ssid, String passphrase, String bssid) {
        status.accept("Joining " + ssid);
        handler.post(() -> wifi.join(ssid, passphrase, bssid, new AccessoryWifi.Result() {
            @Override
            public void joined(String iface, String mac) {
                status.accept("On " + ssid + "; starting CarPlay");
                Native.wifiJoined(handle, iface, mac);
            }

            @Override
            public void failed(String reason) {
                status.accept("Couldn't join " + ssid + ": " + reason);
                Native.wifiJoined(handle, null, reason);
            }
        }));
    }

    @Override
    public void startVideo(int width, int height, int fps, int viewX, int viewY, int viewWidth,
            int viewHeight, boolean hevc) {
        streamed = true;
        status.accept("Streaming at " + width + "x" + height);
        handler.post(() -> {
            touch = Variant.touch(context, viewX, viewY, viewWidth, viewHeight);
            stream = new ScreenStream(context, handle, this::stop);
            try {
                stream.start(width, height, fps, viewX, viewY, viewWidth, viewHeight, hevc);
            } catch (IOException | RuntimeException e) {
                Log.e(TAG, "Could not start the screen stream", e);
                stop();
            }
        });
    }

    @Override
    public void requestKeyframe() {
        ScreenStream s = stream;
        if (s != null) {
            s.requestKeyframe();
        }
    }

    @Override
    public void touch(int action, int pointer, float x, float y) {
        Touch t = touch;
        if (t != null) {
            t.touch(action, pointer, x, y);
        }
    }

    @Override
    public void ended(String error) {
        Log.i(TAG, "Session ended" + (error == null ? "" : ": " + error));
        status.accept("CarPlay ended" + (error == null ? "" : " (" + error + ")")
                + "; waiting for the unit");
        handler.post(() -> {
            if (stream != null) {
                stream.stop();
                stream = null;
            }
            touch = null;
            wifi.leave();
            Bluetooth.closeQuietly(socket);
            listener.ended(this);
        });
    }
}
