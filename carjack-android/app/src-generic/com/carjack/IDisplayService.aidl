package com.carjack;

import android.view.Surface;

/** {@link DisplayService}, run by Shizuku with the shell user's privileges. */
interface IDisplayService {
    /** A trusted display of this size drawing into {@code surface}; returns its id, or -1. */
    int create(in Surface surface, int width, int height, int density) = 1;

    /** {@link Touch} actions, in the display's pixels. */
    void touch(int action, float x, float y) = 2;

    /** Start {@code component} ("pkg/.Activity", or a package) on the display; empty for home. */
    void launch(String component) = 3;

    void release() = 4;

    /** Shizuku's teardown call. */
    void destroy() = 16777114;
}
