package io.github.cfraz89.slipstream;

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

    static final String PREFS = "slipstream";
    static final int NOTIFICATION_ICON = R.drawable.ic_sl_notify;
    static final String LOG_FILE = "slipstream.log";
    /** The installed app's ID, for Shizuku's service component, which is built statically. */
    static final String APP_ID = "io.github.cfraz89.slipstream";

    private static java.lang.Process logcat;

    /**
     * Our own log, kept in files/slipstream.log (and .1-.9) under Android/data, so a session away
     * from adb can be read afterwards - days later: info and up only, the core's chatter trimmed,
     * 40 MB in all. -T: from now, not the whole ring buffer again after each restart.
     */
    static void prepare(Context context) {
        // Not linked yet: puts back a lock delay a crash left held.
        prefs(context).edit().putBoolean("linked", false).apply();
        applyLockHold(context);
        java.io.File dir = context.getExternalFilesDir(null);
        if (dir == null || logcat != null) {
            return;
        }
        try {
            logcat = new ProcessBuilder("logcat", "-v", "time", "-T", "1", "-f",
                    new java.io.File(dir, LOG_FILE).getPath(), "-r", "4096", "-n", "9", "*:I")
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

    /** For {@link MainActivity} to show beside the switch, with Retry where it applies. */
    static void onStatus(Context context, String text, boolean retryable) {
        prefs(context).edit().putString("status", text).putBoolean("retryable", retryable).apply();
    }

    /** Whether a unit's link is up, for {@link MainActivity}'s Bluetooth shortcut. */
    static void onLinked(Context context, boolean linked) {
        prefs(context).edit().putBoolean("linked", linked).apply();
        applyLockHold(context);
    }

    private static final String LOCK_AFTER = "lock_screen_lock_after_timeout";
    /** A day: in effect, no locking on screen-off while the unit is connected. */
    private static final String HELD_LOCK_AFTER = Integer.toString(24 * 60 * 60 * 1000);

    /** Whether the phone's panel goes dark rather than the phone sleeping; off until proven. */
    static boolean keepsPanel(Context context) {
        return prefs(context).getBoolean("keep_panel", false);
    }

    /** Whether to keep the phone unlocked while a unit's connected; the user's choice. */
    static boolean holdsUnlock(Context context) {
        return prefs(context).getBoolean("hold_unlock", true);
    }

    /**
     * While a unit's connected (and the user wants it), the screen going off doesn't lock the
     * phone: the lock screen would hide the apps on the unit's display, which shares its display
     * group. Done as Smart Lock's trusted devices do, by holding off the lock after screen-off;
     * the power button still locks if it's set to lock instantly. Off the caller's thread.
     */
    static void applyLockHold(Context context) {
        Context app = context.getApplicationContext();
        boolean hold = prefs(app).getBoolean("linked", false) && holdsUnlock(app)
                && BootReceiver.isOn(app);
        AccessoryWifi.afterShell(() -> holdLock(app, hold));
    }

    /** Swaps the lock delay for {@link #HELD_LOCK_AFTER}, keeping the user's to put back. */
    private static synchronized void holdLock(Context context, boolean hold) {
        SharedPreferences prefs = prefs(context);
        if (hold == prefs.getBoolean("lock_held", false)) {
            return;
        }
        if (hold) {
            String unknown = new String("?");
            String original = ShizukuDisplay.secureSetting(LOCK_AFTER, unknown);
            if (original == unknown || !ShizukuDisplay.setSecureSetting(LOCK_AFTER, HELD_LOCK_AFTER)) {
                return;
            }
            // Unset is kept as absent, and put back as unset.
            prefs.edit().putBoolean("lock_held", true).putString("lock_after_saved", original)
                    .commit();
            Log.i(TAG, "Holding off the lock while the unit's connected (was " + original + ")");
        } else {
            if (!ShizukuDisplay.setSecureSetting(LOCK_AFTER,
                    prefs.getString("lock_after_saved", null))) {
                return;
            }
            prefs.edit().putBoolean("lock_held", false).remove("lock_after_saved").commit();
            Log.i(TAG, "Lock delay put back");
        }
    }

    /** The notification's Stop is the app's Turn off. */
    static void onStopRequested(Context context) {
        BootReceiver.setOn(context, false);
    }

    /** How long the service's own teardown gets before the process goes. */
    private static final long STOP_GRACE_MS = 1500;

    /**
     * Stop means stopped: once the teardown has had its moment the process ends, taking the
     * native session, the listeners and any network request with it - unless one of its screens
     * is up, which would look like a crash. Shizuku goes only once the WiFi is handed back.
     */
    static void onServiceDestroyed(Service service) {
        Context app = service.getApplicationContext();
        AccessoryWifi.afterShell(() -> {
            holdLock(app, false);
            ShizukuDisplay.unbind();
            new Handler(Looper.getMainLooper()).postDelayed(() -> {
                android.app.ActivityManager.RunningAppProcessInfo state =
                        new android.app.ActivityManager.RunningAppProcessInfo();
                android.app.ActivityManager.getMyMemoryState(state);
                if (state.importance
                        <= android.app.ActivityManager.RunningAppProcessInfo.IMPORTANCE_VISIBLE) {
                    return;
                }
                if (logcat != null) {
                    logcat.destroy();
                }
                Process.killProcess(Process.myPid());
            }, STOP_GRACE_MS);
        });
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

    /** No paging for apps: Slipstream dials the unit's iAP service instead. */
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

    /**
     * The MAC {@code iface} joined with, which apps can't read and the unit may check against the
     * station on its AP; null without Shizuku.
     */
    static String interfaceMac(String iface) {
        return ShizukuDisplay.interfaceMac(iface);
    }

    /**
     * The phone's WiFi switched over to the unit's AP, through Shizuku: one channel for the radio rather than two
     * time-sliced. False where it can't, for the local-only request instead. Blocks.
     */
    static boolean joinPrimary(String ssid, String passphrase) {
        return ShizukuDisplay.joinPrimary(ssid, passphrase);
    }

    /** Back off the unit's AP; with a null SSID, just unlocks switching. Blocks. */
    static void leavePrimary(String ssid) {
        ShizukuDisplay.leavePrimary(ssid);
    }

    /** The built-in launcher's home screen, which the unit gets unless another app is chosen. */
    private static final String LAUNCHER = APP_ID + "/" + APP_ID + ".launcher.HomeActivity";

    /** The chosen app while it's still installed, else the launcher. */
    private static String appFor(Context context) {
        String app = setting(context, "app", "");
        if (app.isEmpty()) {
            return LAUNCHER;
        }
        if (app.equals(Previews.HOME)) {
            android.content.ComponentName home = homeApp(context);
            return home != null ? home.flattenToString() : LAUNCHER;
        }
        try {
            context.getPackageManager().getPackageInfo(app.split("/")[0], 0);
            return app;
        } catch (android.content.pm.PackageManager.NameNotFoundException e) {
            Log.w(TAG, app + " isn't installed; the unit gets the launcher");
            return LAUNCHER;
        }
    }

    /** The phone's own home app, or null where none is set as the default. */
    static android.content.ComponentName homeApp(Context context) {
        android.content.pm.ResolveInfo info = context.getPackageManager().resolveActivity(
                new Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_HOME),
                android.content.pm.PackageManager.MATCH_DEFAULT_ONLY);
        // With no default, the chooser resolves; it isn't a home app.
        if (info == null || info.activityInfo == null
                || info.activityInfo.packageName.equals("android")) {
            return null;
        }
        return new android.content.ComponentName(info.activityInfo.packageName,
                info.activityInfo.name);
    }

    /** A display of the unit's own size with the chosen app on it, or null without Shizuku. */
    static Runnable openDisplay(Context context, int width, int height, int density,
            Surface target) {
        Runnable close = ShizukuDisplay.open(target, width, height, density, appFor(context),
                keepsPanel(context));
        if (close == null) {
            Log.e(TAG, "No display: Shizuku isn't running or hasn't let Slipstream in");
        }
        return close;
    }

    /** Onto the display, 1:1. */
    static Touch touch(Context context, int viewX, int viewY, int viewWidth, int viewHeight) {
        return (action, pointer, panelX, panelY) -> {
            IDisplayService display = ShizukuDisplay.active;
            if (display == null) {
                return;
            }
            try {
                display.touch(action, pointer, panelX - viewX, panelY - viewY);
            } catch (android.os.RemoteException e) {
                Log.w(TAG, "Touch lost", e);
            }
        };
    }

    private Variant() {}
}
