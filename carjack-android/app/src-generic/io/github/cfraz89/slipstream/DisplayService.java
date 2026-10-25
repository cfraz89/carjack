package io.github.cfraz89.slipstream;

import android.content.Context;
import android.content.ContextWrapper;
import android.hardware.display.DisplayManager;
import android.hardware.display.VirtualDisplay;
import android.util.Log;
import android.view.InputEvent;
import android.view.MotionEvent;
import android.view.Surface;

import java.lang.reflect.Method;

/**
 * Runs in a process Shizuku starts as the shell user, which may create trusted displays and inject
 * input - what lets Slipstream give the unit a display of its own resolution with apps running on it,
 * rather than a mirror of the phone. Hidden APIs are reached by reflection; that is allowed here.
 */
public final class DisplayService extends IDisplayService.Stub {
    private static final String TAG = "carjack";
    private static final String SHELL = "com.android.shell";

    // Private (no PUBLIC) and in the default group (no OWN_DISPLAY_GROUP, which this Android
    // ignores anyway): together that makes it never-blank, rendering while the phone sleeps. No
    // SHOULD_SHOW_SYSTEM_DECORATIONS either; the launcher brings its own taskbar.
    private static final int FLAGS = DisplayManager.VIRTUAL_DISPLAY_FLAG_OWN_CONTENT_ONLY
            | 1 << 6 // SUPPORTS_TOUCH
            | 1 << 8 // DESTROY_CONTENT_ON_REMOVAL
            | 1 << 10 // TRUSTED
            | 1 << 14; // OWN_FOCUS: its own input focus, apart from the phone's

    private final Context context;
    private VirtualDisplay display;
    /** The phone kept awake while the display is up. */
    private StayAwake awake;
    /** The phone's screen dark but the phone awake, so the unit's touches still land. */
    private PanelKeeper panel;
    private Fingers fingers = new Fingers();

    public DisplayService() {
        this(null);
    }

    public DisplayService(Context context) {
        this.context = context;
        startSystemLog();
        PanelKeeper.restoreLeftovers();
    }

    /** Where the system log goes: Downloads, so it can be sent on without adb or a shell app. */
    private static final String SYSTEM_LOG = "/sdcard/Download/slipstream-system.log";

    private Process systemLog;

    /** What logs info worth keeping: Slipstream's own lines, and the parts a session leans on. */
    private static final String[] SYSTEM_LOG_TAGS = {
        "carjack", "AndroidRuntime", "ActivityManager", "ActivityTaskManager", "WindowManager",
        "PowerManagerService", "DisplayManagerService", "DisplayDeviceRepository",
        "KeyguardViewMediator", "WifiClientModeImpl", "WifiNetworkFactory",
        "WifiConnectivityManager", "WifiNative", "ConnectivityService", "BluetoothAdapter",
        "BluetoothManagerService", "AdapterService", "UserServiceManager", "MediaCodec",
        "CCodec",
    };

    /** The system log's files, for the app to send on. */
    @Override
    public android.os.ParcelFileDescriptor systemLog(int part) {
        String[] suffixes = {"", ".1", ".2", ".3"};
        if (part < 0 || part >= suffixes.length) {
            return null;
        }
        try {
            return android.os.ParcelFileDescriptor.open(new java.io.File(SYSTEM_LOG
                    + suffixes[part]), android.os.ParcelFileDescriptor.MODE_READ_ONLY);
        } catch (java.io.FileNotFoundException e) {
            return null;
        }
    }

    /**
     * The phone's own log - WiFi, connectivity, Bluetooth, power, displays - which the app's log
     * can't see, while Slipstream is running: warnings from everything, the rest only from what
     * bears on a session, so days of it fit. Four 8 MB files.
     */
    private void startSystemLog() {
        // The last service's logcat outlives it, and two writers would rotate the files over each
        // other: it goes first. -T keeps the new one from replaying what the last one wrote.
        run("pkill", "-f", SYSTEM_LOG);
        try {
            java.util.List<String> command = new java.util.ArrayList<>(java.util.List.of(
                    "logcat", "-b", "main,system,crash", "-v", "time", "-T", "1",
                    "-f", SYSTEM_LOG, "-r", "8192", "-n", "3", "*:W"));
            for (String tag : SYSTEM_LOG_TAGS) {
                command.add(tag + ":I");
            }
            systemLog = new ProcessBuilder(command).start();
        } catch (java.io.IOException e) {
            Log.w(TAG, "No system log", e);
        }
    }

    /** The shell's package: the display service checks the caller's package against its uid. */
    private Context shellContext() {
        return new ContextWrapper(context) {
            @Override
            public String getPackageName() {
                return SHELL;
            }

            @Override
            public String getOpPackageName() {
                return SHELL;
            }
        };
    }

    @Override
    public synchronized int create(Surface surface, int width, int height, int density) {
        release();
        try {
            DisplayManager dm = DisplayManager.class.getDeclaredConstructor(Context.class)
                    .newInstance(shellContext());
            display = dm.createVirtualDisplay("slipstream", width, height, density, surface, FLAGS);
            fingers = new Fingers();
            int id = display.getDisplay().getDisplayId();
            Log.i(TAG, "Display " + id + " at " + width + "x" + height + ", " + density + "dpi");
            awake = new StayAwake(shellContext());
            awake.start();
            keyboardHere(id);
            return id;
        } catch (ReflectiveOperationException | RuntimeException e) {
            Log.e(TAG, "Could not create a display", e);
            return -1;
        }
    }

    @Override
    public synchronized void touch(int action, int pointer, float x, float y) {
        if (display == null) {
            return;
        }
        MotionEvent event = fingers.event(action, pointer, x, y);
        if (event == null) {
            return;
        }
        try {
            InputEvent.class.getMethod("setDisplayId", int.class)
                    .invoke(event, display.getDisplay().getDisplayId());
            Class<?> global = Class.forName("android.hardware.input.InputManagerGlobal");
            Object input = global.getMethod("getInstance").invoke(null);
            Method inject = global.getMethod("injectInputEvent", InputEvent.class, int.class);
            inject.invoke(input, event, 0 /* INJECT_INPUT_EVENT_MODE_ASYNC */);
        } catch (ReflectiveOperationException e) {
            Log.w(TAG, "Could not inject a touch", e);
        } finally {
            event.recycle();
        }
    }

    /**
     * The keyboard on this display, for what's typed into here: by default Android shows it on
     * the phone's (DISPLAY_IME_POLICY_FALLBACK_DISPLAY). Needs INTERNAL_SYSTEM_WINDOW, which the
     * shell has, and a trusted display, which this is.
     */
    private static void keyboardHere(int displayId) {
        try {
            android.os.IBinder binder = (android.os.IBinder) Class.forName("android.os.ServiceManager")
                    .getMethod("getService", String.class).invoke(null, "window");
            Object windows = Class.forName("android.view.IWindowManager$Stub")
                    .getMethod("asInterface", android.os.IBinder.class).invoke(null, binder);
            windows.getClass().getMethod("setDisplayImePolicy", int.class, int.class)
                    .invoke(windows, displayId, 0 /* DISPLAY_IME_POLICY_LOCAL */);
        } catch (ReflectiveOperationException | RuntimeException e) {
            Log.w(TAG, "The keyboard stays on the phone", e);
        }
    }

    @Override
    public void allowNotificationListener(String component) {
        run("cmd", "notification", "allow_listener", component);
    }

    /** From the trust service's dump: the current user's line reads trustState=TRUSTED. */
    @Override
    public boolean trusted() {
        for (String line : run("dumpsys", "trust").split("\n")) {
            if (line.contains("(current)") && line.contains("trustState=")) {
                return line.contains("trustState=TRUSTED");
            }
        }
        return false;
    }

    /** A key going down or up on the display, for its focused window (or, media keys, the player). */
    @Override
    public synchronized void key(int code, boolean down, int meta) {
        if (display == null) {
            return;
        }
        long now = android.os.SystemClock.uptimeMillis();
        android.view.KeyEvent event = new android.view.KeyEvent(now, now,
                down ? android.view.KeyEvent.ACTION_DOWN : android.view.KeyEvent.ACTION_UP, code, 0,
                meta, android.view.KeyCharacterMap.VIRTUAL_KEYBOARD, 0, 0,
                android.view.InputDevice.SOURCE_KEYBOARD);
        try {
            InputEvent.class.getMethod("setDisplayId", int.class)
                    .invoke(event, display.getDisplay().getDisplayId());
            Class<?> global = Class.forName("android.hardware.input.InputManagerGlobal");
            Object input = global.getMethod("getInstance").invoke(null);
            global.getMethod("injectInputEvent", InputEvent.class, int.class)
                    .invoke(input, event, 0 /* INJECT_INPUT_EVENT_MODE_ASYNC */);
        } catch (ReflectiveOperationException e) {
            Log.w(TAG, "Could not inject key " + code, e);
        }
    }

    @Override
    public synchronized void launch(String component) {
        if (display == null) {
            return;
        }
        String id = Integer.toString(display.getDisplay().getDisplayId());
        if (!component.contains("/")) {
            // A package: its launcher activity, as the last line resolve-activity prints.
            String resolved = run("cmd", "package", "resolve-activity", "--brief", "-c",
                    "android.intent.category.LAUNCHER", component).trim();
            component = resolved.substring(resolved.lastIndexOf('\n') + 1);
            if (!component.contains("/")) {
                Log.w(TAG, "No launcher activity in " + resolved);
                return;
            }
        }
        run("am", "start", "--display", id, "-n", component);
    }

    /** Hands-free voice search, an activity, where the assistant key's overlay is the phone's. */
    @Override
    public synchronized void voice() {
        if (display == null) {
            return;
        }
        run("am", "start", "--display", Integer.toString(display.getDisplay().getDisplayId()),
                "-a", "android.intent.action.VOICE_COMMAND", "-f", "0x10000000");
    }

    @Override
    public void cycleBluetooth() {
        run("cmd", "bluetooth_manager", "disable");
        run("cmd", "bluetooth_manager", "wait-for-state:STATE_OFF");
        run("cmd", "bluetooth_manager", "enable");
        run("cmd", "bluetooth_manager", "wait-for-state:STATE_ON");
    }

    @Override
    public String bluetoothAddress() {
        return run("settings", "get", "secure", "bluetooth_address").trim();
    }

    @Override
    public void grant(String pkg, String[] permissions) {
        for (String permission : permissions) {
            run("pm", "grant", pkg, permission);
        }
    }

    /**
     * From dumpsys wifi, which the shell may read where sysfs is closed to it: each client mode's
     * `mLinkProperties {InterfaceName: <iface> ...` is followed by its
     * `mWifiInfo ..., MAC: <the address it joined with>, ...`.
     */
    @Override
    public String interfaceMac(String iface) {
        String inInterface = null;
        for (String line : run("dumpsys", "wifi").split("\n")) {
            line = line.trim();
            if (line.startsWith("mLinkProperties {InterfaceName: ")) {
                inInterface = line.substring("mLinkProperties {InterfaceName: ".length())
                        .split(" ", 2)[0];
            } else if (line.startsWith("mWifiInfo ") && iface.equals(inInterface)) {
                java.util.regex.Matcher mac = java.util.regex.Pattern
                        .compile("MAC: ([0-9a-fA-F:]{17})").matcher(line);
                if (mac.find() && !"02:00:00:00:00:00".equals(mac.group(1))) {
                    return mac.group(1);
                }
            }
        }
        return null;
    }

    @Override
    public void enableBluetooth() {
        run("cmd", "bluetooth_manager", "enable");
        run("cmd", "bluetooth_manager", "wait-for-state:STATE_ON");
    }

    private static final long PRIMARY_JOIN_MS = 20_000;
    private static final long PRIMARY_POLL_MS = 500;

    /**
     * The phone's WiFi switched over to the unit's AP, so the radio stays on its channel rather than
     * time-slicing with home WiFi on another. Switching to another saved network while connected
     * is locked out (the associated network selection override); autojoin stays off for it, so
     * it's never joined outside a session.
     */
    @Override
    public boolean joinPrimary(String ssid, String passphrase) {
        run("cmd", "wifi", "set-network-selection-config", "enabled", "enabled", "-a", "2");
        boolean open = passphrase == null || passphrase.isEmpty();
        if (open) {
            run("cmd", "wifi", "connect-network", ssid, "open", "-d");
        } else {
            run("cmd", "wifi", "connect-network", ssid, "wpa2", passphrase, "-d");
        }
        String connected = "Wifi is connected to \"" + ssid + "\"";
        long deadline = android.os.SystemClock.uptimeMillis() + PRIMARY_JOIN_MS;
        while (android.os.SystemClock.uptimeMillis() < deadline) {
            if (run("cmd", "wifi", "status").contains(connected)) {
                return true;
            }
            android.os.SystemClock.sleep(PRIMARY_POLL_MS);
        }
        Log.w(TAG, "WiFi not switched over to " + ssid + " after " + PRIMARY_JOIN_MS + "ms");
        leavePrimary(ssid);
        return false;
    }

    @Override
    public void leavePrimary(String ssid) {
        if (ssid != null) {
            // list-networks rows: "<id> <ssid> <security>".
            for (String line : run("cmd", "wifi", "list-networks").split("\n")) {
                String[] cols = line.trim().split("\\s+");
                if (cols.length >= 3 && cols[0].matches("\\d+")
                        && line.contains(" " + ssid + " ")) {
                    run("cmd", "wifi", "forget-network", cols[0]);
                }
            }
        }
        run("cmd", "wifi", "set-network-selection-config", "enabled", "enabled", "-a", "0");
    }

    /*
     * The wallpaper calls below are hidden, and need READ_WALLPAPER_INTERNAL, which the shell
     * has: they're how a home screen of the unit's size would frame the phone's wallpaper, from
     * the original rather than the phone's own portrait crop of it.
     */

    @Override
    public android.os.ParcelFileDescriptor wallpaperOriginal() {
        try {
            return (android.os.ParcelFileDescriptor) android.app.WallpaperManager.class
                    .getMethod("getWallpaperFile", int.class, boolean.class)
                    .invoke(wallpapers(), android.app.WallpaperManager.FLAG_SYSTEM, false);
        } catch (ReflectiveOperationException | RuntimeException e) {
            Log.w(TAG, "No original wallpaper", e);
            return null;
        }
    }

    @Override
    public int[] wallpaperCrop(int width, int height) {
        try {
            @SuppressWarnings("unchecked")
            java.util.List<android.graphics.Rect> crops =
                    (java.util.List<android.graphics.Rect>) android.app.WallpaperManager.class
                            .getMethod("getBitmapCrops", java.util.List.class, int.class,
                                    boolean.class)
                            .invoke(wallpapers(),
                                    java.util.List.of(new android.graphics.Point(width, height)),
                                    android.app.WallpaperManager.FLAG_SYSTEM, true);
            if (crops == null || crops.isEmpty()) {
                return null;
            }
            android.graphics.Rect crop = crops.get(0);
            return new int[] {crop.left, crop.top, crop.right, crop.bottom};
        } catch (ReflectiveOperationException | RuntimeException e) {
            Log.w(TAG, "No wallpaper crop for " + width + "x" + height, e);
            return null;
        }
    }

    @Override
    public String secureSetting(String key) {
        String value = run("settings", "get", "secure", key).trim();
        return value.isEmpty() || value.equals("null") ? null : value;
    }

    @Override
    public void setSecureSetting(String key, String value) {
        if (value == null) {
            run("settings", "delete", "secure", key);
        } else {
            run("settings", "put", "secure", key, value);
        }
    }

    private android.app.WallpaperManager wallpapers() {
        return android.app.WallpaperManager.getInstance(shellContext());
    }

    private static String run(String... command) {
        try {
            Process process = new ProcessBuilder(command).redirectErrorStream(true).start();
            String out = new String(process.getInputStream().readAllBytes());
            process.waitFor();
            return out;
        } catch (java.io.IOException e) {
            // Not the arguments: a WiFi join's include the password.
            Log.w(TAG, "Could not run " + command[0], e);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        }
        return "";
    }

    @Override
    public synchronized void keepPanel(boolean keep) {
        if (!keep || display == null) {
            if (panel != null) {
                panel.stop();
                panel = null;
            }
            return;
        }
        if (panel == null) {
            try {
                panel = new PanelKeeper(shellContext());
                panel.start();
            } catch (ReflectiveOperationException | RuntimeException e) {
                Log.w(TAG, "No panel keeping; the phone sleeps as usual", e);
                panel = null;
            }
        }
    }

    @Override
    public synchronized void release() {
        if (panel != null) {
            panel.stop();
            panel = null;
        }
        if (awake != null) {
            awake.stop();
            awake = null;
        }
        if (display != null) {
            display.release();
            display = null;
        }
    }

    @Override
    public void destroy() {
        release();
        if (systemLog != null) {
            systemLog.destroy();
        }
        System.exit(0);
    }
}
