package io.github.cfraz89.slipstream;

import android.app.Application;

import io.github.cfraz89.slipstream.launcher.Wallpapers;

/**
 * Starts crash reports, and hands the launcher what only the app can reach: the phone's
 * wallpaper, through Shizuku.
 */
public final class SlipstreamApplication extends Application {
    @Override
    public void onCreate() {
        super.onCreate();
        Reports.apply(this);
        Wallpapers.setPhoneFramed(ShizukuDisplay::framedWallpaper);
    }
}
