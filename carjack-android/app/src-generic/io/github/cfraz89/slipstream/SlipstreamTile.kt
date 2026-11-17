package io.github.cfraz89.slipstream

import android.content.SharedPreferences
import android.service.quicksettings.Tile
import android.service.quicksettings.TileService

/** The mode switched from Quick Settings - on is Open mode - with where the listening is up to beneath. */
class SlipstreamTile : TileService() {
    private val changed = SharedPreferences.OnSharedPreferenceChangeListener { _, key ->
        if (key == "mode" || key == "status" || key == "listening") show()
    }

    override fun onStartListening() {
        Variant.prefs(this).registerOnSharedPreferenceChangeListener(changed)
        show()
    }

    override fun onStopListening() {
        Variant.prefs(this).unregisterOnSharedPreferenceChangeListener(changed)
    }

    override fun onClick() {
        OpenMode.setOn(this, !OpenMode.isOn(this))
    }

    private fun show() {
        val tile = qsTile ?: return
        tile.state = if (OpenMode.isOn(this)) Tile.STATE_ACTIVE else Tile.STATE_INACTIVE
        tile.label = if (OpenMode.isOn(this)) "Open mode" else "Android Auto"
        tile.subtitle = if (OpenMode.isListening(this)) {
            Variant.setting(this, "status", SlipstreamService.WAITING)
        } else {
            "Off"
        }
        tile.updateTile()
    }
}
