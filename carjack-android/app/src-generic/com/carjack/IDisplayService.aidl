package com.carjack;

import android.view.Surface;

/** {@link DisplayService}, run by Shizuku with the shell user's privileges. */
interface IDisplayService {
    /** A trusted display of this size drawing into {@code surface}; returns its id, or -1. */
    int create(in Surface surface, int width, int height, int density) = 1;

    /** {@link Touch} actions, in the display's pixels. */
    void touch(int action, float x, float y) = 2;

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

    /** Shizuku's teardown call. */
    void destroy() = 16777114;
}
