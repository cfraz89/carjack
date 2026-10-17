package com.carjack;

import android.app.Activity;
import android.app.Service;
import android.bluetooth.BluetoothAdapter;
import android.bluetooth.BluetoothDevice;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.pm.ServiceInfo;
import android.hardware.display.VirtualDisplay;
import android.os.Handler;
import android.os.Looper;
import android.os.Process;
import android.util.Log;
import android.view.Surface;

/**
 * The generic build: an ordinary app on someone's phone. What the platform build does directly is
 * asked of the user instead - permissions, discoverability, pairing, screen capture and touch
 * control - and the screen is the phone's own, mirrored.
 */
final class Variant {
    private static final String TAG = "carjack";

    static final String PREFS = "carjack";
    static final String EXTRA_RESULT_CODE = "com.carjack.RESULT_CODE";
    static final String EXTRA_RESULT_DATA = "com.carjack.RESULT_DATA";

    static void prepare(Context context) {}

    /** Screen capture comes later, with the consent {@link MainActivity} passes on. */
    static int foregroundServiceTypes() {
        return ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE;
    }

    static int onStartCommand(Service service, Intent intent) {
        int code = intent == null ? 0 : intent.getIntExtra(EXTRA_RESULT_CODE, 0);
        Intent data = intent == null ? null : intent.getParcelableExtra(EXTRA_RESULT_DATA, Intent.class);
        if (code == Activity.RESULT_OK && data != null) {
            ((CarjackService) service).promote(ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE
                    | ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION);
            Projection.start(service, code, data);
        }
        // Screen capture can't survive a restart without asking again.
        return Service.START_NOT_STICKY;
    }

    /** How long the service's own teardown gets before the process goes. */
    private static final long STOP_GRACE_MS = 1500;

    /**
     * Stop means stopped: once the teardown has had its moment the process ends, taking the
     * native session, the listeners and any network request with it.
     */
    static void onServiceDestroyed(Service service) {
        Projection.stop();
        ShizukuDisplay.unbind();
        new Handler(Looper.getMainLooper()).postDelayed(
                () -> Process.killProcess(Process.myPid()), STOP_GRACE_MS);
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

    /**
     * Apps only see a placeholder for their own address, which the unit is told; the user can
     * copy the real one from the phone's settings.
     */
    static String bluetoothAddress(Context context, BluetoothAdapter adapter) {
        String address = setting(context, "bluetooth_address", "").trim();
        return address.isEmpty() ? adapter.getAddress() : address;
    }

    /**
     * A display of the unit's own size through Shizuku when it's set up, else a mirror of the
     * phone's screen.
     */
    static Runnable openDisplay(Context context, int width, int height, int density,
            Surface target) {
        if (usesShizuku(context)) {
            Runnable close = ShizukuDisplay.open(target, width, height, density,
                    setting(context, "app", ""));
            if (close != null) {
                return close;
            }
            Log.w(TAG, "No Shizuku display; mirroring the phone's screen");
        }
        VirtualDisplay display = Projection.display(width, height, density, target);
        return () -> Projection.detach(display);
    }

    static boolean usesShizuku(Context context) {
        return "shizuku".equals(setting(context, "display", "mirror"))
                && ShizukuDisplay.available();
    }

    /** Onto the Shizuku display 1:1 while there is one, else onto the mirrored screen. */
    static Touch touch(Context context, int viewX, int viewY, int viewWidth, int viewHeight) {
        GestureTouch mirrored = new GestureTouch(context, viewX, viewY, viewWidth, viewHeight);
        return (action, panelX, panelY) -> {
            IDisplayService display = ShizukuDisplay.active;
            if (display == null) {
                mirrored.touch(action, panelX, panelY);
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
