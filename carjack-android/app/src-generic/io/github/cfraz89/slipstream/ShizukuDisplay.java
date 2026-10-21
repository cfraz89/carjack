package io.github.cfraz89.slipstream;

import android.content.ComponentName;
import android.content.ServiceConnection;
import android.content.pm.PackageManager;
import android.os.IBinder;
import android.os.RemoteException;
import android.util.Log;
import android.view.Surface;

import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;

import rikka.shizuku.Shizuku;

/**
 * A display of the unit's own size with apps running on it, made by {@link DisplayService} through
 * Shizuku. Used in place of the screen mirror where Shizuku is running and has let Slipstream in.
 */
final class ShizukuDisplay {
    private static final String TAG = "carjack";
    private static final long BIND_TIMEOUT_S = 10;

    private static final Shizuku.UserServiceArgs ARGS = new Shizuku.UserServiceArgs(
            new ComponentName(Variant.APP_ID, DisplayService.class.getName()))
            .daemon(false)
            .processNameSuffix("display")
            .debuggable(false)
            .version(22);

    private static volatile IDisplayService service;
    private static CountDownLatch bound = new CountDownLatch(1);
    /** Set while a display is up; touches go to it. */
    static volatile IDisplayService active;

    private static final ServiceConnection CONNECTION = new ServiceConnection() {
        @Override
        public void onServiceConnected(ComponentName name, IBinder binder) {
            service = IDisplayService.Stub.asInterface(binder);
            bound.countDown();
        }

        @Override
        public void onServiceDisconnected(ComponentName name) {
            service = null;
            active = null;
            bound = new CountDownLatch(1);
        }
    };

    /** Shizuku is running and has granted Slipstream access. */
    static boolean available() {
        try {
            return Shizuku.pingBinder()
                    && Shizuku.checkSelfPermission() == PackageManager.PERMISSION_GRANTED;
        } catch (RuntimeException e) {
            return false;
        }
    }

    /**
     * A display drawing into {@code target} with {@code app} (a component or package) on it;
     * returns what closes it, or null where Shizuku can't make one. {@code keepPanel}: the phone's
     * panel goes dark rather than the phone sleeping, meanwhile.
     */
    static Runnable open(Surface target, int width, int height, int density, String app,
            boolean keepPanel) {
        IDisplayService s = bind();
        if (s == null) {
            return null;
        }
        try {
            if (s.create(target, width, height, density) < 0) {
                return null;
            }
            s.launch(app);
            Log.i(TAG, "Display up; the phone's panel " + (keepPanel
                    ? "goes dark instead of the phone sleeping" : "sleeps with the phone, as usual"));
            if (keepPanel) {
                s.keepPanel(true);
            }
            active = s;
            return () -> {
                active = null;
                try {
                    s.release();
                } catch (RemoteException ignored) {
                }
            };
        } catch (RemoteException e) {
            Log.w(TAG, "Shizuku display service failed", e);
            return null;
        }
    }

    /** Bluetooth off and on through Shizuku; false where Shizuku can't. */
    static boolean cycleBluetooth() {
        IDisplayService s = available() ? bind() : null;
        if (s == null) {
            return false;
        }
        try {
            s.cycleBluetooth();
            return true;
        } catch (RemoteException e) {
            Log.w(TAG, "Shizuku couldn't cycle Bluetooth", e);
            return false;
        }
    }

    /** A network interface's MAC through Shizuku, or null. */
    static String interfaceMac(String iface) {
        IDisplayService s = available() ? bind() : null;
        if (s == null) {
            return null;
        }
        try {
            String mac = s.interfaceMac(iface);
            return mac != null && mac.matches("([0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}") ? mac : null;
        } catch (RemoteException e) {
            return null;
        }
    }

    /** {@link IDisplayService#joinPrimary}; false where Shizuku can't. Blocks up to ~20 s. */
    static boolean joinPrimary(String ssid, String passphrase) {
        IDisplayService s = available() ? bind() : null;
        if (s == null) {
            return false;
        }
        try {
            return s.joinPrimary(ssid, passphrase);
        } catch (RemoteException e) {
            Log.w(TAG, "Shizuku couldn't join " + ssid, e);
            return false;
        }
    }

    static void leavePrimary(String ssid) {
        IDisplayService s = available() ? bind() : null;
        if (s == null) {
            return;
        }
        try {
            s.leavePrimary(ssid);
        } catch (RemoteException e) {
            Log.w(TAG, "Shizuku couldn't leave " + ssid, e);
        }
    }

    /**
     * The phone's wallpaper as the system would frame it for a {@code width}x{@code height}
     * display, about that size; null where Shizuku can't read it (a live wallpaper, say). Blocks.
     */
    static android.graphics.Bitmap framedWallpaper(int width, int height) {
        IDisplayService s = available() ? bind() : null;
        if (s == null) {
            return null;
        }
        try (android.os.ParcelFileDescriptor original = s.wallpaperOriginal()) {
            if (original == null) {
                return null;
            }
            android.graphics.BitmapRegionDecoder decoder =
                    android.graphics.BitmapRegionDecoder.newInstance(original);
            int[] c = s.wallpaperCrop(width, height);
            android.graphics.Rect crop = c != null && c.length == 4
                    ? new android.graphics.Rect(c[0], c[1], c[2], c[3])
                    : new android.graphics.Rect(0, 0, decoder.getWidth(), decoder.getHeight());
            crop.intersect(0, 0, decoder.getWidth(), decoder.getHeight());
            android.graphics.BitmapFactory.Options options =
                    new android.graphics.BitmapFactory.Options();
            options.inSampleSize = 1;
            while (crop.width() / (options.inSampleSize * 2) >= width
                    && crop.height() / (options.inSampleSize * 2) >= height) {
                options.inSampleSize *= 2;
            }
            Log.i(TAG, "Wallpaper for " + width + "x" + height + ": " + crop + " of "
                    + decoder.getWidth() + "x" + decoder.getHeight());
            android.graphics.Bitmap bitmap = decoder.decodeRegion(crop, options);
            decoder.recycle();
            return bitmap;
        } catch (RemoteException | java.io.IOException | RuntimeException e) {
            Log.w(TAG, "No framed wallpaper", e);
            return null;
        }
    }

    /**
     * {@link IDisplayService#secureSetting}; {@code fallback} where Shizuku can't say, which
     * callers tell from a real answer by identity.
     */
    static String secureSetting(String key, String fallback) {
        IDisplayService s = available() ? bind() : null;
        if (s == null) {
            return fallback;
        }
        try {
            return s.secureSetting(key);
        } catch (RemoteException e) {
            return fallback;
        }
    }

    /** False where Shizuku couldn't set it. */
    static boolean setSecureSetting(String key, String value) {
        IDisplayService s = available() ? bind() : null;
        if (s == null) {
            return false;
        }
        try {
            s.setSecureSetting(key, value);
            return true;
        } catch (RemoteException e) {
            return false;
        }
    }

    /** The phone's system log into {@code dir}; nothing where Shizuku isn't running. */
    static void copySystemLog(String dir) {
        IDisplayService s = available() ? bind() : null;
        if (s == null) {
            return;
        }
        try {
            s.copySystemLog(dir);
        } catch (RemoteException e) {
            Log.w(TAG, "Shizuku couldn't copy the system log", e);
        }
    }

    /** The phone's real Bluetooth address through Shizuku, or null. */
    static String bluetoothAddress() {
        IDisplayService s = available() ? bind() : null;
        if (s == null) {
            return null;
        }
        try {
            String address = s.bluetoothAddress();
            return address.matches("([0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}") ? address : null;
        } catch (RemoteException e) {
            return null;
        }
    }

    /**
     * What the user would otherwise be asked for: the app's runtime permissions, and Bluetooth
     * on. Blocks while Shizuku starts the service, so not on the main thread.
     */
    static void setUp(String pkg, String[] permissions, boolean enableBluetooth) {
        IDisplayService s = available() ? bind() : null;
        if (s == null) {
            return;
        }
        try {
            s.grant(pkg, permissions);
            if (enableBluetooth) {
                s.enableBluetooth();
            }
        } catch (RemoteException e) {
            Log.w(TAG, "Shizuku couldn't set Slipstream up", e);
        }
    }

    private static IDisplayService bind() {
        if (service == null) {
            try {
                Shizuku.bindUserService(ARGS, CONNECTION);
                bound.await(BIND_TIMEOUT_S, TimeUnit.SECONDS);
            } catch (RuntimeException e) {
                Log.w(TAG, "Could not start the Shizuku display service", e);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
            }
        }
        return service;
    }

    static void unbind() {
        active = null;
        try {
            Shizuku.unbindUserService(ARGS, CONNECTION, true);
        } catch (RuntimeException ignored) {
        }
        service = null;
        bound = new CountDownLatch(1);
    }

    private ShizukuDisplay() {}
}
