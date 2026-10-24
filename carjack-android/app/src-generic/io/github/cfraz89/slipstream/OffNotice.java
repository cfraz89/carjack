package io.github.cfraz89.slipstream;

import android.app.Notification;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.Context;
import android.content.Intent;

/** While Slipstream's off, the notification that turns it back on; the service's own when on. */
final class OffNotice {
    private static final int ID = 2;
    /** Swiped away: it comes back, as the service's does. */
    static final String ACTION_SHOW = "io.github.cfraz89.slipstream.SHOW_OFF";

    static void show(Context context) {
        if (BootReceiver.isOn(context)) {
            return;
        }
        SlipstreamService.createChannel(context);
        PendingIntent on = PendingIntent.getForegroundService(context, 0,
                new Intent(context, SlipstreamService.class)
                        .setAction(SlipstreamService.ACTION_START),
                PendingIntent.FLAG_IMMUTABLE);
        PendingIntent again = PendingIntent.getBroadcast(context, 0,
                new Intent(context, BootReceiver.class).setAction(ACTION_SHOW),
                PendingIntent.FLAG_IMMUTABLE);
        Notification notice = new Notification.Builder(context, SlipstreamService.CHANNEL)
                .setContentTitle(context.getApplicationInfo()
                        .loadLabel(context.getPackageManager()))
                .setContentText("Off")
                .setSmallIcon(Variant.NOTIFICATION_ICON)
                .setOngoing(true)
                .setContentIntent(Variant.openApp(context))
                .setDeleteIntent(again)
                .addAction(new Notification.Action.Builder(null, "Turn on", on).build())
                .build();
        context.getSystemService(NotificationManager.class).notify(ID, notice);
    }

    static void hide(Context context) {
        context.getSystemService(NotificationManager.class).cancel(ID);
    }

    private OffNotice() {}
}
