package io.github.cfraz89.slipstream;

import android.content.Context;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import android.os.PowerManager;
import android.util.Log;

/**
 * Keeps the phone's CPU from suspending for as long as the unit has a display, leaving its screen
 * to do as it would. Runs in the Shizuku process, as the shell user.
 */
final class StayAwake {
    private static final String TAG = "carjack";

    private final Context shell;
    private PowerManager.WakeLock wakeLock;

    StayAwake(Context shell) {
        this.shell = shell;
    }

    void start() {
        try {
            wakeLock = powerManager().newWakeLock(PowerManager.PARTIAL_WAKE_LOCK,
                    "carjack:display");
            wakeLock.acquire();
        } catch (ReflectiveOperationException | RuntimeException e) {
            Log.w(TAG, "Could not keep the phone awake", e);
        }
    }

    void stop() {
        if (wakeLock != null) {
            wakeLock.release();
            wakeLock = null;
        }
    }

    /** One that names the shell, which the wake lock is attributed to. */
    private PowerManager powerManager() throws ReflectiveOperationException {
        Object service = Class.forName("android.os.IPowerManager$Stub")
                .getMethod("asInterface", IBinder.class)
                .invoke(null, Class.forName("android.os.ServiceManager")
                        .getMethod("getService", String.class)
                        .invoke(null, Context.POWER_SERVICE));
        Looper looper = Looper.getMainLooper() != null ? Looper.getMainLooper() : Looper.myLooper();
        return PowerManager.class.getDeclaredConstructor(Context.class,
                        Class.forName("android.os.IPowerManager"),
                        Class.forName("android.os.IThermalService"), Handler.class)
                .newInstance(shell, service, null, looper != null ? new Handler(looper) : null);
    }
}
