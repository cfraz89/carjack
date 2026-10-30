package io.github.cfraz89.slipstream;

import android.os.ParcelFileDescriptor;
import android.view.Surface;

/** {@link DisplayService}, run by Shizuku with the shell user's privileges. */
interface IDisplayService {
    /** A trusted display of this size drawing into {@code surface}; returns its id, or -1. */
    int create(in Surface surface, int width, int height, int density) = 1;

    /** {@link Touch} actions, a pointer id per finger, in the display's pixels. */
    void touch(int action, int pointer, float x, float y) = 2;

    /** Start {@code component} ("pkg/.Activity", or a package) on the display. */
    void launch(String component) = 3;

    void release() = 4;

    /** Bluetooth off and back on, returning once it's on. */
    void cycleBluetooth() = 5;

    /** The phone's Bluetooth address, which apps aren't given. */
    String bluetoothAddress() = 6;

    /** Grant {@code pkg} these runtime permissions. */
    void grant(String pkg, in String[] permissions) = 7;

    void enableBluetooth() = 8;

    /** The MAC address of network interface {@code iface}, or null. */
    String interfaceMac(String iface) = 9;

    /**
     * Switch the phone's WiFi over to {@code ssid}, with switching away locked while connected;
     * true once on it.
     */
    boolean joinPrimary(String ssid, String passphrase) = 10;

    /** Forget {@code ssid} (if not null) and unlock switching, so the phone goes back. */
    void leavePrimary(String ssid) = 11;

    /** The phone's wallpaper as it was set, uncropped; null for a live or default one. */
    ParcelFileDescriptor wallpaperOriginal() = 12;

    /**
     * Where the system would crop that original for a display this size, as left, top, right,
     * bottom; null where it can't say.
     */
    int[] wallpaperCrop(int width, int height) = 13;

    /** A Settings.Secure value, or null where unset. */
    String secureSetting(String key) = 14;

    /** Sets a Settings.Secure value; null deletes it, back to the default. */
    void setSecureSetting(String key, String value) = 15;

    /** Part {@code part} (0 newest) of the phone's system log, open to read; null if none. */
    ParcelFileDescriptor systemLog(int part) = 17;

    /** An Android key code going down or up on the display, with KeyEvent meta state. */
    void key(int code, boolean down, int meta) = 18;

    /** Whether a trust agent (Smart Lock's trusted devices, say) has the phone unlocked. */
    boolean trusted() = 19;

    /** Enables a notification listener ("pkg/class"), as its settings switch would. */
    void allowNotificationListener(String component) = 20;

    /** The phone's voice assistant, opened on the display rather than over the phone's screen. */
    void voice() = 21;

    /** The package of the app showing on the display, or null. */
    String topPackage() = 22;

    /** Wakes the phone and takes away a lock screen that needs no PIN (a trusted device's). */
    void dismissKeyguard() = 23;

    /** Starts an activity ("pkg/class") on the phone's own screen. */
    void launchOnPhone(String component) = 24;

    /**
     * The display a session left ({@link #detach}), drawing into {@code surface} again at this
     * size; returns its id, or -1 where there's none.
     */
    int resume(in Surface surface, int width, int height, int density) = 25;

    /** Off its surface but kept, apps and all, for a session that comes back soon; released after a while. */
    void detach() = 26;

    /** Shizuku's teardown call. */
    void destroy() = 16777114;
}
