package io.github.cfraz89.slipstream;

import android.content.Context;
import android.hardware.display.DisplayManager;
import android.os.PowerManager;
import android.os.SystemClock;
import android.util.Log;
import android.view.Display;

import java.io.BufferedReader;
import java.io.File;
import java.io.IOException;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;

/**
 * The phone's screen going dark without the phone going to sleep, while the unit's display is
 * up: asleep, Android drops the touches sent to the unit, whose display shares the phone's
 * display group. Android's own timeout is held off; in its place, once the phone's touchscreen
 * has been idle that long the panel is switched off ({@code cmd display power-off}), and a touch
 * on it brings it back. The power button still sleeps the phone as ever. Runs in the shell.
 */
final class PanelKeeper {
    private static final String TAG = "carjack";
    private static final String TIMEOUT = "screen_off_timeout";
    private static final String BRIGHTNESS_MODE = "screen_brightness_mode";
    /** Never, in effect. */
    private static final String HELD_TIMEOUT = Integer.toString(Integer.MAX_VALUE);
    private static final long DEFAULT_TIMEOUT_MS = 30_000;
    private static final long TICK_MS = 1_000;
    /** The settings to put back, kept on disk so a crash can't leave the phone never sleeping. */
    private static final File SAVED = new File("/data/local/tmp/slipstream-panel");

    private final PowerManager power;
    private final DisplayManager displays;
    private Thread ticker;
    private Process touches;
    private volatile long lastTouch;
    private volatile boolean panelOff;
    private long idleMs;
    private String savedTimeout = "";
    /** The user's brightness mode, held while the panel is dark. */
    private String brightnessWhileDark;

    PanelKeeper(Context shellContext) throws ReflectiveOperationException {
        power = (PowerManager) shellContext.getSystemService(Context.POWER_SERVICE);
        displays = DisplayManager.class.getDeclaredConstructor(Context.class)
                .newInstance(shellContext);
    }

    /** Puts back what a keeper that never got to stop left changed. */
    static void restoreLeftovers() {
        if (!SAVED.exists()) {
            return;
        }
        try {
            String[] saved = new String(Files.readAllBytes(SAVED.toPath()),
                    StandardCharsets.UTF_8).split("\n", -1);
            put(TIMEOUT, saved[0]);
            // Only there when the panel was dark: the mode to go back to.
            if (saved.length > 1 && !saved[1].isEmpty()) {
                put(BRIGHTNESS_MODE, saved[1]);
                run("cmd", "display", "power-reset", "0");
            }
            Log.i(TAG, "Put back the screen timeout a stopped session left held");
        } catch (IOException e) {
            Log.w(TAG, "Couldn't read " + SAVED, e);
        }
        SAVED.delete();
    }

    synchronized void start() {
        if (ticker != null) {
            return;
        }
        restoreLeftovers();
        String timeout = get(TIMEOUT);
        savedTimeout = timeout == null ? "" : timeout;
        save(null);
        if (!SAVED.exists()) {
            Log.w(TAG, "Can't keep the screen timeout safe; leaving it alone");
            return;
        }
        idleMs = parse(timeout, DEFAULT_TIMEOUT_MS);
        put(TIMEOUT, HELD_TIMEOUT);
        lastTouch = SystemClock.uptimeMillis();
        watchTouches();
        ticker = new Thread(this::tick, "slipstream-panel");
        ticker.start();
        Log.i(TAG, "Keeping the phone awake; its panel goes off after " + idleMs + "ms idle");
    }

    synchronized void stop() {
        if (ticker == null) {
            return;
        }
        ticker.interrupt();
        ticker = null;
        if (touches != null) {
            touches.destroy();
            touches = null;
        }
        if (panelOff) {
            lit();
        }
        restoreLeftovers();
    }

    private void tick() {
        while (!Thread.currentThread().isInterrupted()) {
            synchronized (this) {
                if (!power.isInteractive()) {
                    // The power button: asleep the ordinary way. Start over once it wakes.
                    if (panelOff) {
                        lit();
                    }
                    lastTouch = SystemClock.uptimeMillis();
                } else if (panelOff) {
                    // A brightness change re-applies the panel's state; keep it off.
                    Display phone = displays.getDisplay(Display.DEFAULT_DISPLAY);
                    if (phone != null && phone.getState() != Display.STATE_OFF) {
                        run("cmd", "display", "power-off", "0");
                    }
                } else if (SystemClock.uptimeMillis() - lastTouch >= idleMs) {
                    dark();
                }
            }
            try {
                Thread.sleep(TICK_MS);
            } catch (InterruptedException e) {
                return;
            }
        }
    }

    /** Off, with manual brightness meanwhile: auto-brightness would keep re-lighting it. */
    private void dark() {
        panelOff = true;
        brightnessWhileDark = get(BRIGHTNESS_MODE);
        save(brightnessWhileDark);
        put(BRIGHTNESS_MODE, "0");
        run("cmd", "display", "power-off", "0");
        Log.i(TAG, "Phone panel off; the unit carries on");
    }

    /** Back as it was: the brightness mode the user had, and the panel's proper state. */
    private void lit() {
        panelOff = false;
        put(BRIGHTNESS_MODE, brightnessWhileDark);
        save(null);
        run("cmd", "display", "power-reset", "0");
    }

    private synchronized void panelOn() {
        if (panelOff) {
            lit();
            Log.i(TAG, "Phone panel back on");
        }
    }

    /** The held timeout, and the brightness mode while dark, for a crash to be undone with. */
    private void save(String brightness) {
        try {
            Files.write(SAVED.toPath(), (savedTimeout + "\n" + (brightness == null ? "" : brightness))
                    .getBytes(StandardCharsets.UTF_8));
        } catch (IOException e) {
            Log.w(TAG, "Couldn't update " + SAVED, e);
        }
    }

    /** Any event from the phone's touchscreen is a touch: the panel comes back, idle restarts. */
    private void watchTouches() {
        String device = touchscreen();
        if (device == null) {
            Log.w(TAG, "No touchscreen to watch; the panel comes back with the power button only");
            return;
        }
        try {
            touches = new ProcessBuilder("getevent", "-q", device).start();
        } catch (IOException e) {
            Log.w(TAG, "Can't watch " + device, e);
            return;
        }
        Process watched = touches;
        new Thread(() -> {
            try (BufferedReader in = new BufferedReader(
                    new InputStreamReader(watched.getInputStream(), StandardCharsets.US_ASCII))) {
                while (in.readLine() != null) {
                    lastTouch = SystemClock.uptimeMillis();
                    if (panelOff) {
                        panelOn();
                    }
                }
            } catch (IOException ignored) {
            }
        }, "slipstream-touches").start();
    }

    /** The input device reporting multi-touch positions, from getevent's device listing. */
    private static String touchscreen() {
        String device = null;
        for (String line : run("getevent", "-pl").split("\n")) {
            if (line.startsWith("add device")) {
                device = line.substring(line.indexOf(':') + 1).trim();
            } else if (device != null && line.contains("ABS_MT_POSITION_X")) {
                return device;
            }
        }
        return null;
    }

    private static long parse(String value, long fallback) {
        try {
            long parsed = Long.parseLong(value);
            return parsed > 0 && parsed < Integer.MAX_VALUE ? parsed : fallback;
        } catch (NumberFormatException e) {
            return fallback;
        }
    }

    /** A system setting, or null where unset. */
    private static String get(String key) {
        String value = run("settings", "get", "system", key).trim();
        return value.isEmpty() || value.equals("null") ? null : value;
    }

    private static void put(String key, String value) {
        if (value == null || value.isEmpty() || value.equals("null")) {
            run("settings", "delete", "system", key);
        } else {
            run("settings", "put", "system", key, value);
        }
    }

    private static String run(String... command) {
        try {
            Process process = new ProcessBuilder(command).redirectErrorStream(true).start();
            String out = new String(process.getInputStream().readAllBytes());
            process.waitFor();
            return out;
        } catch (IOException e) {
            Log.w(TAG, "Could not run " + String.join(" ", command), e);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        }
        return "";
    }
}
