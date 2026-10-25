package io.github.cfraz89.slipstream

import android.app.Application
import io.github.cfraz89.slipstream.launcher.Wallpapers

/**
 * Starts crash reports, and hands the launcher what only the app can reach: the phone's
 * wallpaper, through Shizuku.
 */
class SlipstreamApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        Reports.apply(this)
        Wallpapers.phoneFramed = ShizukuDisplay::framedWallpaper
    }
}
