package io.github.cfraz89.slipstream

import android.app.Notification
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent

/** While Slipstream's off, the notification that turns it back on; the service's own when on. */
object OffNotice {
    private const val ID = 2
    /** Swiped away: it comes back, as the service's does. */
    const val ACTION_SHOW = "io.github.cfraz89.slipstream.SHOW_OFF"

    fun show(context: Context) {
        if (BootReceiver.isOn(context)) return
        SlipstreamService.createChannel(context)
        val on = PendingIntent.getForegroundService(
            context, 0,
            Intent(context, SlipstreamService::class.java).setAction(SlipstreamService.ACTION_START),
            PendingIntent.FLAG_IMMUTABLE,
        )
        val again = PendingIntent.getBroadcast(
            context, 0, Intent(context, BootReceiver::class.java).setAction(ACTION_SHOW),
            PendingIntent.FLAG_IMMUTABLE,
        )
        val notice = Notification.Builder(context, SlipstreamService.CHANNEL)
            .setContentTitle(context.applicationInfo.loadLabel(context.packageManager))
            .setContentText("Off")
            .setSmallIcon(Variant.NOTIFICATION_ICON)
            .setOngoing(true)
            .setContentIntent(Variant.openApp(context))
            .setDeleteIntent(again)
            .addAction(Notification.Action.Builder(null, "Turn on", on).build())
            .build()
        context.getSystemService(NotificationManager::class.java).notify(ID, notice)
    }

    fun hide(context: Context) {
        context.getSystemService(NotificationManager::class.java).cancel(ID)
    }
}
