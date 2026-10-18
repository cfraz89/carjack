package com.carjack;

import android.app.Service;
import android.bluetooth.BluetoothAdapter;
import android.bluetooth.BluetoothDevice;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.pm.ServiceInfo;
import android.os.Handler;
import android.os.Looper;
import android.os.Process;
import android.util.Log;
import android.view.Surface;

/**
 * The generic build: an ordinary app on someone's phone, with Shizuku standing in for the
 * privileges the platform build has - the unit's display, touch on it, Bluetooth cycling and the
 * phone's Bluetooth address. Permissions and pairing are asked of the user.
 */
final class Variant {
    private static final String TAG = "carjack";

    static final String PREFS = "carjack";

    private static java.lang.Process logcat;

    /**
     * Our own log, kept in files/carjack.log (and .1-.3) under Android/data, so a session away
     * from adb can be read afterwards.
     */
    static void prepare(Context context) {
        java.io.File dir = context.getExternalFilesDir(null);
        if (dir == null || logcat != null) {
            return;
        }
        try {
            logcat = new ProcessBuilder("logcat", "-v", "time", "-f",
                    new java.io.File(dir, "carjack.log").getPath(), "-r", "2048", "-n", "3")
                    .start();
        } catch (java.io.IOException e) {
            Log.w(TAG, "No log file", e);
        }
    }

    static int foregroundServiceTypes() {
        return ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE;
    }

    static int onStartCommand(Service service, Intent intent) {
        return Service.START_STICKY;
    }

    /** For {@link MainActivity} to show beside the switch. */
    static void onStatus(Context context, String text) {
        prefs(context).edit().putString("status", text).apply();
    }

    /** The notification's Stop is the app's Turn off. */
    static void onStopRequested(Context context) {
        BootReceiver.setOn(context, false);
    }

    /** How long the service's own teardown gets before the process goes. */
    private static final long STOP_GRACE_MS = 1500;

    /**
     * Stop means stopped: once the teardown has had its moment the process ends, taking the
     * native session, the listeners and any network request with it.
     */
    static void onServiceDestroyed(Service service) {
        ShizukuDisplay.unbind();
        new Handler(Looper.getMainLooper()).postDelayed(() -> {
            if (logcat != null) {
                logcat.destroy();
            }
            Process.killProcess(Process.myPid());
        }, STOP_GRACE_MS);
    }

    static String setting(Context context, String key, String fallback) {
        return prefs(context).getString(key, fallback);
    }

    static SharedPreferences prefs(Context context) {
        return context.getSharedPreferences(PREFS, Context.MODE_PRIVATE);
    }

    /** Asked for from {@link MainActivity}, for as long as Android allows. */
    static void makeDiscoverable(BluetoothAdapter adapter) {}

    /** The user answers the system's dialog. */
    static boolean confirmPairing(BluetoothDevice device, Intent intent) {
        Log.i(TAG, "Pairing request from " + device.getAddress() + "; the user confirms it");
        return false;
    }

    /** No paging for apps: carjack dials the unit's iAP service instead. */
    static boolean canPage() {
        return false;
    }

    /**
     * Apps can neither see the link nor page a device, so it is taken as up and left to the
     * phone's own profiles, which reconnect a bonded unit.
     */
    static boolean isConnected(BluetoothDevice device) {
        return true;
    }

    /** Apps can't page a device; the phone's own profiles reconnect it. */
    static boolean connect(BluetoothDevice device) {
        return false;
    }

    static void disconnect(BluetoothDevice device) {}

    /** Bluetooth off and on, for a fresh link. */
    static boolean cycleBluetooth() {
        return ShizukuDisplay.cycleBluetooth();
    }

    /**
     * Apps only see a placeholder for their own address; Shizuku reads the real one, which some
     * units (the CFMOTO) need in CarPlayAvailability.
     */
    static String bluetoothAddress(Context context, BluetoothAdapter adapter) {
        String address = ShizukuDisplay.bluetoothAddress();
        if (address == null) {
            Log.w(TAG, "No Bluetooth address from Shizuku; the unit gets a placeholder");
            return adapter.getAddress();
        }
        return address;
    }

    /** What apps are given for their own address, which some units take better. */
    private static final String PLACEHOLDER = "02:00:00:00:00:00";

    /**
     * The address {@code unit} is told. Units differ: the CFMOTO is thought to want the real one,
     * while the bench AUTO BOX never brings AirPlay up for it but does for the placeholder. So
     * each unit keeps whichever last worked, swapping after a session that died at AirPlay.
     */
    static String addressFor(Context context, String address, BluetoothDevice unit) {
        boolean placeholder = prefs(context).getBoolean(placeholderKey(unit), false);
        String chosen = placeholder ? PLACEHOLDER : address;
        Log.i(TAG, "Telling " + unit.getAddress() + " we are " + chosen);
        return chosen;
    }

    static void onSessionEnded(Context context, BluetoothDevice unit, String error) {
        if (error != null && error.contains("AirPlay endpoint")) {
            String key = placeholderKey(unit);
            prefs(context).edit().putBoolean(key, !prefs(context).getBoolean(key, false)).apply();
        }
    }

    private static String placeholderKey(BluetoothDevice unit) {
        return "placeholder_" + unit.getAddress();
    }

    /** A display of the unit's own size with the chosen app on it, or null without Shizuku. */
    static Runnable openDisplay(Context context, int width, int height, int density,
            Surface target) {
        Runnable close = ShizukuDisplay.open(target, width, height, density,
                setting(context, "app", ""));
        if (close == null) {
            Log.e(TAG, "No display: Shizuku isn't running or hasn't let carjack in");
        }
        return close;
    }

    /** Onto the display, 1:1. */
    static Touch touch(Context context, int viewX, int viewY, int viewWidth, int viewHeight) {
        return (action, panelX, panelY) -> {
            IDisplayService display = ShizukuDisplay.active;
            if (display == null) {
                return;
            }
            try {
                display.touch(action, panelX - viewX, panelY - viewY);
            } catch (android.os.RemoteException e) {
                Log.w(TAG, "Touch lost", e);
            }
        };
    }

    private Variant() {}
}
