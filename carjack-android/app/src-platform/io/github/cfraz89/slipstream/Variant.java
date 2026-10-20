package io.github.cfraz89.slipstream;

import android.Manifest;
import android.app.Service;
import android.bluetooth.BluetoothAdapter;
import android.bluetooth.BluetoothDevice;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.content.pm.ServiceInfo;
import android.hardware.display.DisplayManager;
import android.hardware.display.VirtualDisplay;
import android.os.Process;
import android.os.SystemProperties;
import android.os.UserHandle;
import android.util.Log;
import android.view.Display;
import android.view.IWindowManager;
import android.view.Surface;
import android.view.WindowManagerGlobal;

import java.time.Duration;

/**
 * The platform build: platform-signed and preinstalled, on a board with nobody at the screen, so
 * everything a user would be asked for is done directly.
 */
final class Variant {
    private static final String TAG = "carjack";

    /** Runtime permissions granted to ourselves, since nobody is there to answer a prompt. */
    private static final String[] RUNTIME_PERMISSIONS = {
        Manifest.permission.BLUETOOTH_CONNECT,
        Manifest.permission.BLUETOOTH_SCAN,
        Manifest.permission.BLUETOOTH_ADVERTISE,
        Manifest.permission.ACCESS_FINE_LOCATION,
        Manifest.permission.ACCESS_COARSE_LOCATION,
        Manifest.permission.NEARBY_WIFI_DEVICES,
        Manifest.permission.POST_NOTIFICATIONS,
    };

    static void prepare(Context context) {
        PackageManager pm = context.getPackageManager();
        for (String permission : RUNTIME_PERMISSIONS) {
            if (context.checkSelfPermission(permission) != PackageManager.PERMISSION_GRANTED) {
                try {
                    pm.grantRuntimePermission(context.getPackageName(), permission,
                            UserHandle.getUserHandleForUid(Process.myUid()));
                } catch (RuntimeException e) {
                    Log.w(TAG, "Could not grant " + permission, e);
                }
            }
        }
    }

    static int foregroundServiceTypes() {
        return ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE;
    }

    /** Started from boot, and restarted if it dies. */
    static int onStartCommand(Service service, Intent intent) {
        return Service.START_STICKY;
    }

    /** The platform app reads sysfs itself. */
    static String interfaceMac(String iface) {
        return null;
    }

    /** No app screen to show these on. */
    static void onStatus(Context context, String text, boolean retryable) {}

    static void onLinked(Context context, boolean linked) {}

    /** Stopped until the next boot. */
    static void onStopRequested(Context context) {}

    static void onServiceDestroyed(Service service) {}

    /** {@code persist.carjack.<key>}. */
    static String setting(Context context, String key, String fallback) {
        return SystemProperties.get("persist.carjack." + key, fallback);
    }

    static void makeDiscoverable(BluetoothAdapter adapter) {
        // Zero is "never times out".
        adapter.setDiscoverableTimeout(Duration.ZERO);
        adapter.setScanMode(BluetoothAdapter.SCAN_MODE_CONNECTABLE_DISCOVERABLE);
    }

    /** Answers the pairing itself; true means the system's dialog is not needed. */
    static boolean confirmPairing(BluetoothDevice device, Intent intent) {
        int variant = intent.getIntExtra(BluetoothDevice.EXTRA_PAIRING_VARIANT, -1);
        Log.i(TAG, "Pairing request from " + device.getAddress() + ", variant " + variant);
        if (variant == BluetoothDevice.PAIRING_VARIANT_PIN) {
            device.setPin("0000");
        } else {
            device.setPairingConfirmation(true);
        }
        return true;
    }

    static boolean canPage() {
        return true;
    }

    static boolean isConnected(BluetoothDevice device) {
        return device.isConnected();
    }

    /** Page the unit; false where this build can't. */
    static boolean connect(BluetoothDevice device) {
        try {
            device.connect();
            return true;
        } catch (RuntimeException e) {
            Log.w(TAG, "Could not page " + device.getAddress(), e);
            return false;
        }
    }

    static void disconnect(BluetoothDevice device) {
        try {
            device.disconnect();
        } catch (RuntimeException e) {
            Log.w(TAG, "Could not disconnect " + device.getAddress(), e);
        }
    }

    /** Never needed: this build pages the unit. */
    static boolean cycleBluetooth() {
        return false;
    }

    static String bluetoothAddress(Context context, BluetoothAdapter adapter) {
        return adapter.getAddress();
    }

    /**
     * The display the stream shows: the default one, resized to the part of the panel it gets and
     * mirrored 1:1 into {@code target}.
     */
    static Runnable openDisplay(Context context, int width, int height, int density,
            Surface target) {
        try {
            IWindowManager wm = WindowManagerGlobal.getWindowManagerService();
            wm.setForcedDisplaySize(Display.DEFAULT_DISPLAY, width, height);
            wm.setForcedDisplayDensityForUser(Display.DEFAULT_DISPLAY, density,
                    UserHandle.USER_CURRENT);
        } catch (Exception e) {
            Log.w(TAG, "Could not resize the display", e);
        }
        VirtualDisplay display = context.getSystemService(DisplayManager.class)
                .createVirtualDisplay("carjack", width, height, density, target,
                        DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR
                                | DisplayManager.VIRTUAL_DISPLAY_FLAG_SECURE);
        return display::release;
    }

    /** Touches in the view rectangle land 1:1 on the resized default display. */
    static Touch touch(Context context, int viewX, int viewY, int viewWidth, int viewHeight) {
        return new TouchInjector(viewX, viewY, viewWidth, viewHeight);
    }

    private Variant() {}
}
