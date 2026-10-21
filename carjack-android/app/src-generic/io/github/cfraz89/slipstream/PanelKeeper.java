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
 * display group, and a lock screen would hide its apps. So for the session Android's own timeout
 * is held off and the power button's short press does nothing; in their place the panel is
 * switched off ({@code cmd display power-off}) once the phone's touchscreen has been idle for the
 * user's timeout, or at a press of the power button. A touch or another press brings it back.
 * Runs in the shell.
 */
final class PanelKeeper {
    private static final String TAG = "carjack";
    private static final String SYSTEM = "system";
    private static final String GLOBAL = "global";
    private static final String TIMEOUT = "screen_off_timeout";
    private static final String BRIGHTNESS_MODE = "screen_brightness_mode";
    private static final String POWER_PRESS = "power_button_short_press";
    /** Never, in effect. */
    private static final String HELD_TIMEOUT = Integer.toString(Integer.MAX_VALUE);
    /** PhoneWindowManager's SHORT_PRESS_POWER_NOTHING: the press is ours to act on. */
    private static final String POWER_PRESS_NOTHING = "0";
    private static final long DEFAULT_TIMEOUT_MS = 30_000;
    private static final long TICK_MS = 1_000;
    /**
     * What to put back, kept on disk so a crash can't leave the phone never sleeping: the
     * timeout, the power press, and the brightness mode while the panel is dark. A line is empty
     * where the setting was unset.
     */
    private static final File SAVED = new File("/data/local/tmp/slipstream-panel");

    private final PowerManager power;
    private final DisplayManager displays;
    private Thread ticker;
    private Process touches;
    private Process keys;
    private volatile long lastTouch;
    private volatile boolean panelOff;
    private long idleMs;
    private String savedTimeout;
    private String savedPowerPress;
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
            put(SYSTEM, TIMEOUT, saved[0]);
            if (saved.length > 1) {
                put(GLOBAL, POWER_PRESS, saved[1]);
            }
            if (saved.length > 2 && !saved[2].isEmpty()) {
                put(SYSTEM, BRIGHTNESS_MODE, saved[2]);
                run("cmd", "display", "power-reset", "0");
                run("input", "keyevent", "KEYCODE_SLEEP");
                run("input", "keyevent", "KEYCODE_WAKEUP");
            }
            Log.i(TAG, "Put back the screen settings a stopped session left held");
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
        savedTimeout = get(SYSTEM, TIMEOUT);
        savedPowerPress = get(GLOBAL, POWER_PRESS);
        if (!save(null)) {
            Log.w(TAG, "Can't keep the screen settings safe; leaving them alone");
            return;
        }
        idleMs = parse(savedTimeout, DEFAULT_TIMEOUT_MS);
        put(SYSTEM, TIMEOUT, HELD_TIMEOUT);
        put(GLOBAL, POWER_PRESS, POWER_PRESS_NOTHING);
        lastTouch = SystemClock.uptimeMillis();
        touches = watch("ABS_MT_POSITION_X", line -> touched());
        keys = watch("KEY_POWER", line -> {
            if (line.contains("KEY_POWER") && line.contains("DOWN")) {
                powerPressed();
            }
        });
        ticker = new Thread(this::tick, "slipstream-panel");
        ticker.start();
        Log.i(TAG, "Keeping the phone awake; its panel goes off after " + idleMs
                + "ms idle or at the power button");
    }

    synchronized void stop() {
        if (ticker == null) {
            return;
        }
        ticker.interrupt();
        ticker = null;
        for (Process watcher : new Process[] {touches, keys}) {
            if (watcher != null) {
                watcher.destroy();
            }
        }
        touches = null;
        keys = null;
        if (panelOff) {
            lit();
        }
        restoreLeftovers();
    }

    private void tick() {
        while (!Thread.currentThread().isInterrupted()) {
            synchronized (this) {
                if (!power.isInteractive()) {
                    // Asleep after all (a long press's Power off, say): start over once awake.
                    if (panelOff) {
                        lit();
                    }
                    lastTouch = SystemClock.uptimeMillis();
                } else if (panelOff) {
                    // A brightness change re-applies the panel's state; keep it off.
                    if (phoneState() != Display.STATE_OFF) {
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

    private synchronized void touched() {
        lastTouch = SystemClock.uptimeMillis();
        if (panelOff) {
            lit();
            Log.i(TAG, "Phone panel back on at a touch");
        }
    }

    /** With the short press left to us: dark if lit, lit if we darkened it. */
    private synchronized void powerPressed() {
        lastTouch = SystemClock.uptimeMillis();
        if (panelOff) {
            lit();
            Log.i(TAG, "Phone panel back on at the power button");
        } else if (power.isInteractive() && phoneState() == Display.STATE_ON) {
            dark();
        }
        // Otherwise asleep: Android wakes it as ever.
    }

    private int phoneState() {
        Display phone = displays.getDisplay(Display.DEFAULT_DISPLAY);
        return phone == null ? Display.STATE_UNKNOWN : phone.getState();
    }

    /** Off, with manual brightness meanwhile: auto-brightness would keep re-lighting it. */
    private void dark() {
        panelOff = true;
        brightnessWhileDark = get(SYSTEM, BRIGHTNESS_MODE);
        save(brightnessWhileDark == null ? "" : brightnessWhileDark);
        put(SYSTEM, BRIGHTNESS_MODE, "0");
        run("cmd", "display", "power-off", "0");
        Log.i(TAG, "Phone panel off; the unit carries on");
    }

    /**
     * Back as it was: the brightness mode the user had, and the panel's proper state. power-reset
     * alone can be refused (silently: "runnable is null") when the display manager has the panel
     * down as on already, so the phone also sleeps and wakes, a moment's dip, which always has
     * the panel brought back up properly.
     */
    private void lit() {
        panelOff = false;
        put(SYSTEM, BRIGHTNESS_MODE, brightnessWhileDark);
        save(null);
        run("cmd", "display", "power-reset", "0");
        run("input", "keyevent", "KEYCODE_SLEEP");
        run("input", "keyevent", "KEYCODE_WAKEUP");
    }

    private boolean save(String brightness) {
        String saved = nonNull(savedTimeout) + "\n" + nonNull(savedPowerPress) + "\n"
                + nonNull(brightness);
        try {
            Files.write(SAVED.toPath(), saved.getBytes(StandardCharsets.UTF_8));
            return true;
        } catch (IOException e) {
            Log.w(TAG, "Couldn't write " + SAVED, e);
            return false;
        }
    }

    /**
     * Follows the input device that reports {@code capability} (in getevent's listing), handing
     * each event line to {@code onLine}; null where there's none.
     */
    private Process watch(String capability, java.util.function.Consumer<String> onLine) {
        String device = inputDevice(capability);
        if (device == null) {
            Log.w(TAG, "No input device with " + capability);
            return null;
        }
        Process process;
        try {
            process = new ProcessBuilder("getevent", "-ql", device).start();
        } catch (IOException e) {
            Log.w(TAG, "Can't watch " + device, e);
            return null;
        }
        new Thread(() -> {
            try (BufferedReader in = new BufferedReader(
                    new InputStreamReader(process.getInputStream(), StandardCharsets.US_ASCII))) {
                String line;
                while ((line = in.readLine()) != null) {
                    onLine.accept(line);
                }
            } catch (IOException ignored) {
            }
        }, "slipstream-" + capability).start();
        return process;
    }

    private static String inputDevice(String capability) {
        String device = null;
        for (String line : run("getevent", "-pl").split("\n")) {
            if (line.startsWith("add device")) {
                device = line.substring(line.indexOf(':') + 1).trim();
            } else if (device != null && line.contains(capability)) {
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

    private static String nonNull(String value) {
        return value == null ? "" : value;
    }

    /** A setting, or null where unset. */
    private static String get(String namespace, String key) {
        String value = run("settings", "get", namespace, key).trim();
        return value.isEmpty() || value.equals("null") ? null : value;
    }

    private static void put(String namespace, String key, String value) {
        if (value == null || value.isEmpty() || value.equals("null")) {
            run("settings", "delete", namespace, key);
        } else {
            run("settings", "put", namespace, key, value);
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
