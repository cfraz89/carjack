package io.github.cfraz89.slipstream

import android.app.Application
import io.github.cfraz89.slipstream.launcher.AppRepository
import io.github.cfraz89.slipstream.launcher.Wallpapers

/**
 * Starts crash reports, and hands the launcher what only the app can reach through Shizuku: the
 * phone's wallpaper, and its apps brought over. A fresh process isn't activated, whatever a killed one left set.
 */
class SlipstreamApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        Activation.setOn(this, false)
        Reports.apply(this)
        Wallpapers.phoneFramed = ShizukuDisplay::framedWallpaper
        AppRepository.bringOver = ShizukuDisplay::bringOver
    }
}
