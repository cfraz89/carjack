package io.github.cfraz89.slipstream

import android.Manifest
import android.app.ForegroundServiceStartNotAllowedException
import android.content.Intent
import android.content.SharedPreferences
import android.content.pm.PackageManager
import android.service.quicksettings.Tile
import android.service.quicksettings.TileService
import android.util.Log

/** Slipstream on or off from Quick Settings, with where it's up to beneath. */
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

    override fun onClick() {
        val on = !BootReceiver.isOn(this)
        // A connected-device service can't start before Bluetooth is granted: the app asks.
        if (on && checkSelfPermission(Manifest.permission.BLUETOOTH_CONNECT) !=
            PackageManager.PERMISSION_GRANTED
        ) {
            Variant.openApp(this)?.let { startActivityAndCollapse(it) }
            return
        }
        val service = Intent(this, SlipstreamService::class.java)
        if (on) {
            try {
                startForegroundService(service)
            } catch (e: ForegroundServiceStartNotAllowedException) {
                Log.w(TAG, "Not allowed to start from the tile; opening the app", e)
                Variant.openApp(this)?.let { startActivityAndCollapse(it) }
                return
            }
        } else {
            stopService(service)
        }
        BootReceiver.setOn(this, on)
        show()
    }

    private fun show() {
        val tile = qsTile ?: return
        val on = BootReceiver.isOn(this)
        tile.state = if (on) Tile.STATE_ACTIVE else Tile.STATE_INACTIVE
        tile.subtitle = if (on) Variant.setting(this, "status", SlipstreamService.WAITING) else "Off"
        tile.updateTile()
    }
}
