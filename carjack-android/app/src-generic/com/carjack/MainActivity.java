package com.carjack;

import android.Manifest;
import android.app.Activity;
import android.bluetooth.BluetoothAdapter;
import android.bluetooth.BluetoothDevice;
import android.bluetooth.BluetoothManager;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.pm.PackageManager;
import android.content.pm.ResolveInfo;
import android.content.res.Configuration;
import android.graphics.Insets;
import android.graphics.Typeface;
import android.graphics.drawable.GradientDrawable;
import android.os.Bundle;
import android.view.Gravity;
import android.view.View;
import android.view.WindowInsets;
import android.widget.AdapterView;
import android.widget.ArrayAdapter;
import android.widget.Button;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.Spinner;
import android.widget.Switch;
import android.widget.TextView;

import java.util.ArrayList;
import java.util.List;

import rikka.shizuku.Shizuku;

/** On/off, what's left to set up, and the few choices there are. */
public final class MainActivity extends Activity {
    private static final int REQUEST_PERMISSIONS = 1;
    private static final int REQUEST_SHIZUKU = 4;
    private static final int DISCOVERABLE_SECONDS = 300;
    private static final String SHIZUKU_PACKAGE = "moe.shizuku.privileged.api";

    private static final String[] PERMISSIONS = {
        Manifest.permission.BLUETOOTH_CONNECT,
        Manifest.permission.BLUETOOTH_SCAN,
        Manifest.permission.BLUETOOTH_ADVERTISE,
        Manifest.permission.NEARBY_WIFI_DEVICES,
        Manifest.permission.POST_NOTIFICATIONS,
    };

    /** Display densities offered, by name; 0 is automatic. */
    private static final String[] SIZES = {"Default", "Smaller", "Larger", "Largest"};
    private static final int[] DENSITIES = {0, 160, 240, 280};

    private Switch power;
    private TextView status;
    private Button retry;
    private Button bluetoothSettings;
    private LinearLayout setup;
    private Spinner bike;
    private Spinner app;
    private final List<String> bikeAddresses = new ArrayList<>();
    private final List<String> appPackages = new ArrayList<>();
    private boolean settingUp;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        if (getActionBar() != null) {
            getActionBar().hide();
        }
        LinearLayout page = new LinearLayout(this);
        page.setOrientation(LinearLayout.VERTICAL);
        page.setPadding(dp(20), dp(24), dp(20), dp(24));

        TextView title = text("carjack", 30);
        title.setTypeface(Typeface.DEFAULT_BOLD);
        page.addView(title);
        TextView subtitle = text("CarPlay from this phone", 15);
        subtitle.setAlpha(0.7f);
        page.addView(subtitle);

        setup = card(page, color(android.R.color.system_accent3_100,
                android.R.color.system_accent3_800));

        LinearLayout main = card(page, cardColor());
        power = new Switch(this);
        power.setText("CarPlay");
        power.setTextSize(22);
        power.setTypeface(Typeface.DEFAULT_BOLD);
        power.setOnClickListener(v -> setOn(power.isChecked()));
        main.addView(power);
        status = text("", 15);
        status.setAlpha(0.75f);
        status.setPadding(0, dp(4), 0, 0);
        main.addView(status);
        LinearLayout actions = new LinearLayout(this);
        retry = new Button(this);
        retry.setText("Retry");
        retry.setAllCaps(false);
        retry.setOnClickListener(v -> startService(
                new Intent(this, CarjackService.class).setAction(CarjackService.ACTION_RETRY)));
        actions.addView(retry);
        bluetoothSettings = new Button(this, null, android.R.attr.borderlessButtonStyle);
        bluetoothSettings.setText("Bluetooth settings");
        bluetoothSettings.setAllCaps(false);
        bluetoothSettings.setOnClickListener(v -> openBluetoothSettings());
        actions.addView(bluetoothSettings);
        main.addView(actions);

        LinearLayout settings = card(page, cardColor());
        bike = new Spinner(this);
        row(settings, "Bike", bike);
        app = new Spinner(this);
        row(settings, "App on the bike", app);
        Spinner size = new Spinner(this);
        row(settings, "Text and icons", size);
        size.setAdapter(adapter(java.util.Arrays.asList(SIZES)));
        size.setSelection(densityIndex());
        size.setOnItemSelectedListener(selected(densityIndex(), i -> Variant.prefs(this).edit()
                .putString("density", DENSITIES[i] == 0 ? "" : Integer.toString(DENSITIES[i]))
                .apply()));
        fillApps();

        Button pair = new Button(this, null, android.R.attr.borderlessButtonStyle);
        pair.setText("Make this phone visible to pair");
        pair.setAllCaps(false);
        pair.setOnClickListener(v -> startActivity(
                new Intent(BluetoothAdapter.ACTION_REQUEST_DISCOVERABLE)
                        .putExtra(BluetoothAdapter.EXTRA_DISCOVERABLE_DURATION,
                                DISCOVERABLE_SECONDS)));
        page.addView(pair);

        ScrollView scroll = new ScrollView(this);
        scroll.setBackgroundColor(color(android.R.color.system_neutral1_10,
                android.R.color.system_neutral1_900));
        scroll.addView(page);
        // Drawn edge to edge, so the bars' insets are ours to keep clear of.
        scroll.setOnApplyWindowInsetsListener((v, insets) -> {
            Insets bars = insets.getInsets(WindowInsets.Type.systemBars());
            v.setPadding(bars.left, bars.top, bars.right, bars.bottom);
            return WindowInsets.CONSUMED;
        });
        setContentView(scroll);
        Shizuku.addRequestPermissionResultListener(shizukuResult);
        Variant.prefs(this).registerOnSharedPreferenceChangeListener(prefsChanged);
    }

    private final Shizuku.OnRequestPermissionResultListener shizukuResult =
            (code, result) -> runOnUiThread(this::refresh);

    /** The service's status, and the switch flipped from the notification. */
    private final SharedPreferences.OnSharedPreferenceChangeListener prefsChanged =
            (prefs, key) -> {
                if ("status".equals(key) || "on".equals(key) || "retryable".equals(key)
                        || "linked".equals(key)) {
                    runOnUiThread(this::showPower);
                }
            };

    @Override
    protected void onDestroy() {
        Shizuku.removeRequestPermissionResultListener(shizukuResult);
        Variant.prefs(this).unregisterOnSharedPreferenceChangeListener(prefsChanged);
        super.onDestroy();
    }

    @Override
    protected void onResume() {
        super.onResume();
        refresh();
    }

    private void refresh() {
        showPower();
        fillBikes();
        showSetup();
        autoSetUp();
    }

    private void showPower() {
        boolean on = BootReceiver.isOn(this);
        power.setChecked(on);
        status.setText(on ? Variant.setting(this, "status", "Waiting for the bike") : "Off");
        SharedPreferences prefs = Variant.prefs(this);
        retry.setVisibility(on && prefs.getBoolean("retryable", false) ? View.VISIBLE : View.GONE);
        // Nothing connected: the phone's own Bluetooth page is where that gets sorted.
        bluetoothSettings.setVisibility(on && !prefs.getBoolean("linked", false)
                ? View.VISIBLE : View.GONE);
    }

    private void openBluetoothSettings() {
        startActivity(new Intent(android.provider.Settings.ACTION_BLUETOOTH_SETTINGS));
    }

    /** One row per thing still missing; the card goes when nothing is. */
    private void showSetup() {
        setup.removeAllViews();
        if (!pingShizuku()) {
            step("Start Shizuku, through Wireless debugging", "Open", v -> {
                Intent open = getPackageManager().getLaunchIntentForPackage(SHIZUKU_PACKAGE);
                if (open != null) {
                    startActivity(open);
                }
            });
        } else if (!ShizukuDisplay.available()) {
            step("Let carjack use Shizuku", "Allow",
                    v -> Shizuku.requestPermission(REQUEST_SHIZUKU));
        }
        // With Shizuku, autoSetUp sees to these.
        if (!ShizukuDisplay.available()) {
            if (!permissionsGranted()) {
                step("Bluetooth, Wi-Fi and notification permissions", "Allow",
                        v -> requestPermissions(PERMISSIONS, REQUEST_PERMISSIONS));
            }
            if (!bluetoothOn()) {
                step("Bluetooth is off", "Turn on", v -> startActivity(
                        new Intent(BluetoothAdapter.ACTION_REQUEST_ENABLE)));
            }
        }
        if (permissionsGranted() && bluetoothOn() && bikeAddresses.size() <= 1) {
            step("No CarPlay unit is paired yet", "Pair", v -> openBluetoothSettings());
        }
        setup.setVisibility(setup.getChildCount() == 0 ? View.GONE : View.VISIBLE);
    }

    /** With Shizuku: grant what's missing and switch Bluetooth on, unasked. */
    private void autoSetUp() {
        boolean grant = !permissionsGranted();
        boolean bluetooth = !bluetoothOn();
        if (settingUp || !(grant || bluetooth) || !ShizukuDisplay.available()) {
            return;
        }
        settingUp = true;
        new Thread(() -> {
            ShizukuDisplay.setUp(getPackageName(), PERMISSIONS, bluetooth);
            runOnUiThread(() -> {
                settingUp = false;
                refresh();
            });
        }, "carjack-setup").start();
    }

    private void setOn(boolean on) {
        // A connected-device foreground service can't start before Bluetooth is granted.
        if (on && !permissionsGranted()) {
            requestPermissions(PERMISSIONS, REQUEST_PERMISSIONS);
            on = false;
        }
        BootReceiver.setOn(this, on);
        Intent service = new Intent(this, CarjackService.class);
        if (on) {
            startForegroundService(service);
        } else {
            stopService(service);
        }
        showPower();
    }

    /** Automatic, then each paired CarPlay unit. */
    private void fillBikes() {
        List<String> names = new ArrayList<>();
        bikeAddresses.clear();
        names.add("Whichever connects");
        bikeAddresses.add("");
        BluetoothAdapter adapter = getSystemService(BluetoothManager.class).getAdapter();
        if (bluetoothOn() && checkSelfPermission(Manifest.permission.BLUETOOTH_CONNECT)
                == PackageManager.PERMISSION_GRANTED) {
            for (BluetoothDevice device : adapter.getBondedDevices()) {
                if (Bluetooth.hostsIap(device.getUuids())) {
                    names.add(device.getName() != null ? device.getName() : device.getAddress());
                    bikeAddresses.add(device.getAddress());
                }
            }
        }
        bike.setOnItemSelectedListener(null);
        bike.setAdapter(adapter(names));
        String target = Variant.setting(this, "target", "");
        int current = 0;
        for (int i = 0; i < bikeAddresses.size(); i++) {
            if (bikeAddresses.get(i).equalsIgnoreCase(target)) {
                current = i;
            }
        }
        bike.setSelection(current);
        bike.setOnItemSelectedListener(selected(current, i -> Variant.prefs(this).edit()
                .putString("target", bikeAddresses.get(i)).apply()));
    }

    /** carjack's launcher, then every app with a launcher icon, by name. */
    private void fillApps() {
        PackageManager pm = getPackageManager();
        List<ResolveInfo> found = pm.queryIntentActivities(
                new Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER), 0);
        List<String[]> apps = new ArrayList<>();
        for (ResolveInfo info : found) {
            String pkg = info.activityInfo.packageName;
            if (!pkg.equals(getPackageName())) {
                apps.add(new String[] {info.loadLabel(pm).toString(), pkg});
            }
        }
        apps.sort((a, b) -> a[0].compareToIgnoreCase(b[0]));
        List<String> names = new ArrayList<>();
        appPackages.clear();
        names.add("Carjack launcher");
        appPackages.add("");
        for (String[] a : apps) {
            names.add(a[0]);
            appPackages.add(a[1]);
        }
        app.setAdapter(adapter(names));
        int chosen = Math.max(appPackages.indexOf(Variant.setting(this, "app", "")), 0);
        app.setSelection(chosen);
        app.setOnItemSelectedListener(selected(chosen, i -> Variant.prefs(this).edit()
                .putString("app", appPackages.get(i)).apply()));
    }

    private int densityIndex() {
        String density = Variant.setting(this, "density", "");
        for (int i = 1; i < DENSITIES.length; i++) {
            if (Integer.toString(DENSITIES[i]).equals(density)) {
                return i;
            }
        }
        return 0;
    }

    private boolean permissionsGranted() {
        for (String permission : PERMISSIONS) {
            if (checkSelfPermission(permission) != PackageManager.PERMISSION_GRANTED) {
                return false;
            }
        }
        return true;
    }

    private boolean bluetoothOn() {
        BluetoothAdapter adapter = getSystemService(BluetoothManager.class).getAdapter();
        return adapter != null && adapter.isEnabled();
    }

    private static boolean pingShizuku() {
        try {
            return Shizuku.pingBinder();
        } catch (RuntimeException e) {
            return false;
        }
    }

    @Override
    public void onRequestPermissionsResult(int requestCode, String[] permissions, int[] results) {
        refresh();
    }

    // Layout helpers.

    private int dp(int value) {
        return Math.round(value * getResources().getDisplayMetrics().density);
    }

    private boolean night() {
        return (getResources().getConfiguration().uiMode & Configuration.UI_MODE_NIGHT_MASK)
                == Configuration.UI_MODE_NIGHT_YES;
    }

    private int color(int light, int dark) {
        return getColor(night() ? dark : light);
    }

    private int cardColor() {
        return color(android.R.color.system_neutral2_50, android.R.color.system_neutral2_800);
    }

    private TextView text(String value, int sp) {
        TextView view = new TextView(this);
        view.setText(value);
        view.setTextSize(sp);
        android.content.res.TypedArray primary =
                obtainStyledAttributes(new int[] {android.R.attr.textColorPrimary});
        view.setTextColor(primary.getColorStateList(0));
        primary.recycle();
        return view;
    }

    private LinearLayout card(LinearLayout parent, int background) {
        LinearLayout card = new LinearLayout(this);
        card.setOrientation(LinearLayout.VERTICAL);
        card.setPadding(dp(20), dp(16), dp(20), dp(16));
        GradientDrawable shape = new GradientDrawable();
        shape.setCornerRadius(dp(24));
        shape.setColor(background);
        card.setBackground(shape);
        LinearLayout.LayoutParams params = new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT);
        params.topMargin = dp(16);
        parent.addView(card, params);
        return card;
    }

    private void row(LinearLayout card, String label, View control) {
        TextView name = text(label, 13);
        name.setAlpha(0.7f);
        name.setPadding(0, card.getChildCount() == 0 ? 0 : dp(12), 0, 0);
        card.addView(name);
        card.addView(control);
    }

    private void step(String what, String action, View.OnClickListener onClick) {
        LinearLayout line = new LinearLayout(this);
        line.setGravity(Gravity.CENTER_VERTICAL);
        TextView label = text(what, 15);
        line.addView(label, new LinearLayout.LayoutParams(0,
                LinearLayout.LayoutParams.WRAP_CONTENT, 1));
        Button button = new Button(this);
        button.setText(action);
        button.setAllCaps(false);
        button.setOnClickListener(onClick);
        line.addView(button);
        setup.addView(line);
    }

    private ArrayAdapter<String> adapter(List<String> items) {
        ArrayAdapter<String> adapter = new ArrayAdapter<>(this,
                android.R.layout.simple_spinner_item, items);
        adapter.setDropDownViewResource(android.R.layout.simple_spinner_dropdown_item);
        return adapter;
    }

    /**
     * Calls back for a change from {@code initial} only: a spinner reports its initial selection
     * once laid out, which would otherwise overwrite a choice missing from today's list.
     */
    private static AdapterView.OnItemSelectedListener selected(int initial,
            java.util.function.IntConsumer onSelected) {
        return new AdapterView.OnItemSelectedListener() {
            private int last = initial;

            @Override
            public void onItemSelected(AdapterView<?> parent, View view, int position, long id) {
                if (position != last) {
                    last = position;
                    onSelected.accept(position);
                }
            }

            @Override
            public void onNothingSelected(AdapterView<?> parent) {}
        };
    }
}
