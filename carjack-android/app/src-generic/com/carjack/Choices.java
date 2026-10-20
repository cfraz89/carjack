package com.carjack;

import android.content.ComponentName;
import android.content.Context;
import android.content.pm.PackageManager;
import android.graphics.drawable.Drawable;

/**
 * What the unit opens on connect, as the "app" setting holds it: "" for carjack's launcher,
 * {@link Previews#HOME} for the phone's home app, else a package. Names and icons for the UI.
 */
final class Choices {
    static final String LAUNCHER = "";

    static String current(Context context) {
        return Variant.setting(context, "app", LAUNCHER);
    }

    static void choose(Context context, String app) {
        Variant.prefs(context).edit().putString("app", app).apply();
    }

    static boolean isApp(String app) {
        return !app.equals(LAUNCHER) && !app.equals(Previews.HOME);
    }

    static String name(Context context, String app) {
        if (app.equals(LAUNCHER)) {
            return "Carjack launcher";
        }
        if (app.equals(Previews.HOME)) {
            return "Home screen";
        }
        return appLabel(context, app);
    }

    /** The detail under the name: whose home screen, or nothing. */
    static String detail(Context context, String app) {
        if (app.equals(Previews.HOME)) {
            ComponentName home = Variant.homeApp(context);
            return home != null ? appLabel(context, home.getPackageName()) : "None set";
        }
        return null;
    }

    static Drawable icon(Context context, String app) {
        PackageManager pm = context.getPackageManager();
        try {
            if (app.equals(Previews.HOME)) {
                ComponentName home = Variant.homeApp(context);
                if (home != null) {
                    return pm.getApplicationIcon(home.getPackageName());
                }
            } else if (isApp(app)) {
                return pm.getApplicationIcon(app.split("/")[0]);
            }
        } catch (PackageManager.NameNotFoundException ignored) {
        }
        return context.getDrawable(app.equals(LAUNCHER) ? R.drawable.ic_launcher
                : app.equals(Previews.HOME) ? R.drawable.ic_cj_home : R.drawable.ic_cj_apps);
    }

    private static String appLabel(Context context, String app) {
        PackageManager pm = context.getPackageManager();
        try {
            return pm.getApplicationLabel(pm.getApplicationInfo(app.split("/")[0], 0)).toString();
        } catch (PackageManager.NameNotFoundException e) {
            return app;
        }
    }

    private Choices() {}
}
