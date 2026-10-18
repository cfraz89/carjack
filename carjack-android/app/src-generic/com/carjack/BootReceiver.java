package com.carjack;

import android.Manifest;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;

/** Back on after a reboot or an update, if the user left carjack on. */
public final class BootReceiver extends BroadcastReceiver {
    @Override
    public void onReceive(Context context, Intent intent) {
        if (isOn(context)
                // A connected-device service can't start without it.
                && context.checkSelfPermission(Manifest.permission.BLUETOOTH_CONNECT)
                        == PackageManager.PERMISSION_GRANTED) {
            context.startForegroundService(new Intent(context, CarjackService.class));
        }
    }

    static boolean isOn(Context context) {
        return Variant.prefs(context).getBoolean("on", false);
    }

    static void setOn(Context context, boolean on) {
        Variant.prefs(context).edit().putBoolean("on", on).apply();
    }
}
