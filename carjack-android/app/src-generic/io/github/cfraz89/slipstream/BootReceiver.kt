package io.github.cfraz89.slipstream

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/** Listening from boot and after an update, where Shizuku is already up; see [OpenMode.listen]. */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        OpenMode.listen(context)
    }
}
