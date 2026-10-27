package io.github.cfraz89.slipstream

import android.Manifest
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager

/** Back on after a reboot or an update, if the user left it on. */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (isOn(context) &&
            // A connected-device service can't start without it.
            context.checkSelfPermission(Manifest.permission.BLUETOOTH_CONNECT) ==
            PackageManager.PERMISSION_GRANTED
        ) {
            context.startForegroundService(Intent(context, SlipstreamService::class.java))
        }
    }

    companion object {
        fun isOn(context: Context) = Variant.prefs(context).getBoolean("on", false)

        fun setOn(context: Context, on: Boolean) {
            Variant.prefs(context).edit().putBoolean("on", on).apply()
        }
    }
}
