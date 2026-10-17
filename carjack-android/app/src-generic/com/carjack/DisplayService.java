package com.carjack;

import android.content.Context;
import android.content.ContextWrapper;
import android.hardware.display.DisplayManager;
import android.hardware.display.VirtualDisplay;
import android.os.SystemClock;
import android.util.Log;
import android.view.InputDevice;
import android.view.InputEvent;
import android.view.MotionEvent;
import android.view.Surface;

import java.lang.reflect.Method;

/**
 * Runs in a process Shizuku starts as the shell user, which may create trusted displays and inject
 * input - what lets carjack give the unit a display of its own resolution with apps running on it,
 * rather than a mirror of the phone. Hidden APIs are reached by reflection; that is allowed here.
 */
public final class DisplayService extends IDisplayService.Stub {
    private static final String TAG = "carjack";
    private static final String SHELL = "com.android.shell";

    private static final int FLAGS = DisplayManager.VIRTUAL_DISPLAY_FLAG_PUBLIC
            | DisplayManager.VIRTUAL_DISPLAY_FLAG_OWN_CONTENT_ONLY
            | 1 << 6 // SUPPORTS_TOUCH
            | 1 << 8 // DESTROY_CONTENT_ON_REMOVAL
            | 1 << 9 // SHOULD_SHOW_SYSTEM_DECORATIONS: a launcher and the system bars
            | 1 << 10 // TRUSTED
            | 1 << 12 // ALWAYS_UNLOCKED
            | 1 << 14; // OWN_FOCUS: its own input focus, apart from the phone's

    private final Context context;
    private VirtualDisplay display;
    private long downTime = -1;

    public DisplayService() {
        this(null);
    }

    public DisplayService(Context context) {
        this.context = context;
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
            display = dm.createVirtualDisplay("carjack", width, height, density, surface, FLAGS);
            int id = display.getDisplay().getDisplayId();
            Log.i(TAG, "Display " + id + " at " + width + "x" + height + ", " + density + "dpi");
            return id;
        } catch (ReflectiveOperationException | RuntimeException e) {
            Log.e(TAG, "Could not create a display", e);
            return -1;
        }
    }

    @Override
    public synchronized void touch(int action, float x, float y) {
        if (display == null) {
            return;
        }
        long now = SystemClock.uptimeMillis();
        int motion;
        switch (action) {
            case Touch.DOWN:
                downTime = now;
                motion = MotionEvent.ACTION_DOWN;
                break;
            case Touch.MOVE:
                motion = MotionEvent.ACTION_MOVE;
                break;
            case Touch.UP:
                motion = MotionEvent.ACTION_UP;
                break;
            default:
                return;
        }
        if (downTime < 0) {
            return;
        }
        MotionEvent.PointerProperties props = new MotionEvent.PointerProperties();
        props.id = 0;
        props.toolType = MotionEvent.TOOL_TYPE_FINGER;
        MotionEvent.PointerCoords coords = new MotionEvent.PointerCoords();
        coords.x = x;
        coords.y = y;
        coords.pressure = motion == MotionEvent.ACTION_UP ? 0 : 1;
        coords.size = 1;
        MotionEvent event = MotionEvent.obtain(downTime, now, motion, 1,
                new MotionEvent.PointerProperties[] {props},
                new MotionEvent.PointerCoords[] {coords}, 0, 0, 1, 1, 0, 0,
                InputDevice.SOURCE_TOUCHSCREEN, 0);
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
        if (motion == MotionEvent.ACTION_UP) {
            downTime = -1;
        }
    }

    @Override
    public synchronized void launch(String component) {
        if (display == null) {
            return;
        }
        String id = Integer.toString(display.getDisplay().getDisplayId());
        if (component == null || component.isEmpty()) {
            run("am", "start", "--display", id, "-a", "android.intent.action.MAIN", "-c",
                    "android.intent.category.SECONDARY_HOME");
            return;
        }
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

    @Override
    public void cycleBluetooth() {
        run("cmd", "bluetooth_manager", "disable");
        run("cmd", "bluetooth_manager", "wait-for-state:STATE_OFF");
        run("cmd", "bluetooth_manager", "enable");
        run("cmd", "bluetooth_manager", "wait-for-state:STATE_ON");
    }

    private static String run(String... command) {
        try {
            Process process = new ProcessBuilder(command).redirectErrorStream(true).start();
            String out = new String(process.getInputStream().readAllBytes());
            process.waitFor();
            return out;
        } catch (java.io.IOException e) {
            Log.w(TAG, "Could not run " + String.join(" ", command), e);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        }
        return "";
    }

    @Override
    public synchronized void release() {
        if (display != null) {
            display.release();
            display = null;
        }
    }

    @Override
    public void destroy() {
        release();
        System.exit(0);
    }
}
