package com.carjack;

import android.content.Context;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import android.os.PowerManager;
import android.util.Log;

import java.lang.reflect.Method;

/**
 * Keeps the phone awake with its own panel dark, for as long as the unit has a display: on this
 * Android every virtual display shares the phone's display group, so it sleeps when the phone
 * does. The phone is locked too, which the unit's always-unlocked display ignores. Runs in the
 * Shizuku process, as the shell user; the panel trick is scrcpy's.
 */
final class StayAwake {
    private static final String TAG = "carjack";
    private static final int POWER_MODE_OFF = 0;
    private static final int POWER_MODE_NORMAL = 2;

    private final Context shell;
    private PowerManager.WakeLock wakeLock;

    StayAwake(Context shell) {
        this.shell = shell;
    }

    void start() {
        try {
            wakeLock = powerManager().newWakeLock(PowerManager.SCREEN_DIM_WAKE_LOCK
                    | PowerManager.ACQUIRE_CAUSES_WAKEUP, "carjack:display");
            wakeLock.acquire();
        } catch (ReflectiveOperationException | RuntimeException e) {
            Log.w(TAG, "Could not keep the phone awake; the unit's display sleeps with it", e);
        }
        lock();
        setPanel(POWER_MODE_OFF);
    }

    void stop() {
        setPanel(POWER_MODE_NORMAL);
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

    /** The phone's own keyguard, so stray touches in a pocket meet the lock screen. */
    private static void lock() {
        try {
            Object wm = Class.forName("android.view.WindowManagerGlobal")
                    .getMethod("getWindowManagerService").invoke(null);
            wm.getClass().getMethod("lockNow", android.os.Bundle.class).invoke(wm, (Object) null);
        } catch (ReflectiveOperationException | RuntimeException e) {
            Log.w(TAG, "Could not lock the phone", e);
        }
    }

    /** Every physical panel's power, through DisplayControl in services.jar. */
    private static void setPanel(int mode) {
        try {
            Class<?> control = displayControl();
            Method setPower = Class.forName("android.view.SurfaceControl")
                    .getMethod("setDisplayPowerMode", IBinder.class, int.class);
            Method token = control.getMethod("getPhysicalDisplayToken", long.class);
            for (long id : (long[]) control.getMethod("getPhysicalDisplayIds").invoke(null)) {
                setPower.invoke(null, token.invoke(null, id), mode);
            }
        } catch (ReflectiveOperationException | RuntimeException e) {
            Log.w(TAG, "Could not switch the phone's panel", e);
        }
    }

    private static Class<?> displayControl;

    private static synchronized Class<?> displayControl() throws ReflectiveOperationException {
        if (displayControl == null) {
            ClassLoader loader = (ClassLoader) Class
                    .forName("com.android.internal.os.ClassLoaderFactory")
                    .getDeclaredMethod("createClassLoader", String.class, String.class,
                            String.class, ClassLoader.class, int.class, boolean.class,
                            String.class)
                    .invoke(null, "/system/framework/services.jar", null, null,
                            ClassLoader.getSystemClassLoader(), 0, true, null);
            Class<?> control = loader.loadClass("com.android.server.display.DisplayControl");
            Method load = Runtime.class.getDeclaredMethod("loadLibrary0", Class.class,
                    String.class);
            load.setAccessible(true);
            load.invoke(Runtime.getRuntime(), control, "android_servers");
            displayControl = control;
        }
        return displayControl;
    }
}
