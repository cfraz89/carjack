package io.github.cfraz89.slipstream

import android.app.ForegroundServiceStartNotAllowedException
import android.app.PendingIntent
import android.content.SharedPreferences
import android.service.quicksettings.Tile
import android.service.quicksettings.TileService
import android.util.Log

/** Slipstream activated or ended from Quick Settings, with where it's up to beneath. */
class SlipstreamTile : TileService() {
    private companion object {
        const val TAG = "carjack"
    }

    private val changed = SharedPreferences.OnSharedPreferenceChangeListener { _, key ->
        if (key == "on" || key == "status") show()
    }

    override fun onStartListening() {
        Variant.prefs(this).registerOnSharedPreferenceChangeListener(changed)
        show()
    }

    override fun onStopListening() {
        Variant.prefs(this).unregisterOnSharedPreferenceChangeListener(changed)
    }

    /** Activating needs the phone unlocked, and opens the dashboard; ending doesn't. */
    override fun onClick() {
        if (Activation.isOn(this)) {
            Activation.stop(this)
            show()
        } else if (isLocked) {
            unlockAndRun(::start)
        } else {
            start()
        }
    }

    private fun start() {
        // Until it's set up, the app's start screen takes over: it leads to settings.
        if (!Activation.ready(this)) {
            Variant.openApp(this)?.let { startActivityAndCollapse(it) }
            return
        }
        try {
            Activation.startService(this)
        } catch (e: ForegroundServiceStartNotAllowedException) {
            Log.w(TAG, "Not allowed to start from the tile; opening the app", e)
            Variant.openApp(this)?.let { startActivityAndCollapse(it) }
            return
        }
        startActivityAndCollapse(
            PendingIntent.getActivity(this, 0, Activation.dashboard(this), PendingIntent.FLAG_IMMUTABLE),
        )
        show()
    }

    private fun show() {
        val tile = qsTile ?: return
        val on = Activation.isOn(this)
        tile.state = if (on) Tile.STATE_ACTIVE else Tile.STATE_INACTIVE
        tile.subtitle = if (on) Variant.setting(this, "status", SlipstreamService.WAITING) else "Off"
        tile.updateTile()
    }
}
