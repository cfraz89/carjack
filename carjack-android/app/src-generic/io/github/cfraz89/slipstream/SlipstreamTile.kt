package io.github.cfraz89.slipstream

import android.content.SharedPreferences
import android.service.quicksettings.Tile
import android.service.quicksettings.TileService

/** The mode switched from Quick Settings - on is Slipstream mode - with where the listening is up to beneath. */
class SlipstreamTile : TileService() {
    private val changed = SharedPreferences.OnSharedPreferenceChangeListener { _, key ->
        if (key == "mode" || key == "status" || key == "listening") show()
    }

    override fun onStartListening() {
        Profiles.global(this).registerOnSharedPreferenceChangeListener(changed)
        Variant.prefs(this).also { profile = it }.registerOnSharedPreferenceChangeListener(changed)
        show()
    }

    override fun onStopListening() {
        Profiles.global(this).unregisterOnSharedPreferenceChangeListener(changed)
        profile?.unregisterOnSharedPreferenceChangeListener(changed)
        profile = null
    }

    /** The selected profile's settings, as listened to: the mode's in them. */
    private var profile: android.content.SharedPreferences? = null

    override fun onClick() {
        SlipstreamMode.setOn(this, !SlipstreamMode.isOn(this))
    }

    private fun show() {
        val tile = qsTile ?: return
        tile.state = if (SlipstreamMode.isOn(this)) Tile.STATE_ACTIVE else Tile.STATE_INACTIVE
        tile.label = if (SlipstreamMode.isOn(this)) "Slipstream mode" else "Android Auto"
        tile.subtitle = if (SlipstreamMode.isListening(this)) {
            Profiles.global(this).getString("status", null) ?: SlipstreamService.WAITING
        } else {
            "Off"
        }
        tile.updateTile()
    }
}
