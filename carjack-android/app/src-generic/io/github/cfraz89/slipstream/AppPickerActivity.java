package io.github.cfraz89.slipstream;

import android.app.Activity;
import android.content.Intent;
import android.content.pm.ApplicationInfo;
import android.content.pm.PackageManager;
import android.content.pm.ResolveInfo;
import android.graphics.drawable.Drawable;
import android.os.Bundle;
import android.view.View;
import android.widget.LinearLayout;

import java.util.ArrayList;
import java.util.HashSet;
import java.util.List;
import java.util.Set;

/** Every app with a launcher icon, by name, for the unit to open on connect. */
public final class AppPickerActivity extends Activity {
    static final String EXTRA_PACKAGE = "package";

    private static final class App {
        final String pkg;
        final String label;
        final Drawable icon;

        App(String pkg, String label, Drawable icon) {
            this.pkg = pkg;
            this.label = label;
            this.icon = icon;
        }
    }

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Ui ui = new Ui(this);
        LinearLayout page = ui.page();
        ui.topBar(page, "Choose an app");
        LinearLayout list = ui.card(page, R.color.sl_surface_container);
        // Off the main thread: a phone's worth of labels and icons takes a moment.
        new Thread(() -> {
            List<App> apps = apps();
            runOnUiThread(() -> {
                if (isDestroyed()) {
                    return;
                }
                String current = Choices.current(this).split("/")[0];
                for (App app : apps) {
                    View check = current.equals(app.pkg)
                            ? ui.icon(R.drawable.ic_sl_check, R.color.sl_primary, 24) : null;
                    ui.row(list, app.icon, false, app.label, null, check, v -> {
                        setResult(RESULT_OK, new Intent().putExtra(EXTRA_PACKAGE, app.pkg));
                        finish();
                    });
                }
            });
        }, "slipstream-apps").start();
    }

    private List<App> apps() {
        PackageManager pm = getPackageManager();
        List<App> apps = new ArrayList<>();
        Set<String> seen = new HashSet<>();
        for (ResolveInfo info : pm.queryIntentActivities(
                new Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER), 0)) {
            ApplicationInfo app = info.activityInfo.applicationInfo;
            if (!app.packageName.equals(getPackageName()) && seen.add(app.packageName)) {
                apps.add(new App(app.packageName, app.loadLabel(pm).toString(),
                        app.loadIcon(pm)));
            }
        }
        apps.sort((a, b) -> a.label.compareToIgnoreCase(b.label));
        return apps;
    }
}
