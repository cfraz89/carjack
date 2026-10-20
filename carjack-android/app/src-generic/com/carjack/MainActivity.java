package com.carjack;

import android.Manifest;
import android.app.Activity;
import android.app.AlertDialog;
import android.bluetooth.BluetoothAdapter;
import android.bluetooth.BluetoothDevice;
import android.bluetooth.BluetoothManager;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.pm.PackageManager;
import android.graphics.Typeface;
import android.os.Bundle;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.widget.ImageView;
import android.widget.LinearLayout;
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
        // The phone's wallpaper, for the launcher on the unit.
        Manifest.permission.READ_MEDIA_IMAGES,
    };

    /** Display densities offered, by name; 0 is automatic. */
    private static final String[] SIZES = {"Default", "Smaller", "Larger", "Largest"};
    private static final int[] DENSITIES = {0, 160, 240, 280};

    private Ui ui;
    private LinearLayout setup;
    private LinearLayout powerCard;
    private Switch power;
    private TextView status;
    private LinearLayout actions;
    private View retry;
    private LinearLayout settings;
    private final List<String> unitNames = new ArrayList<>();
    private final List<String> unitAddresses = new ArrayList<>();
    private boolean settingUp;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        ui = new Ui(this);
        LinearLayout page = ui.page();

        LinearLayout header = new LinearLayout(this);
        header.setGravity(Gravity.CENTER_VERTICAL);
        header.setPadding(ui.dp(8), ui.dp(24), ui.dp(8), ui.dp(12));
        ImageView logo = new ImageView(this);
        logo.setImageResource(R.drawable.ic_cj_app);
        header.addView(logo, new LinearLayout.LayoutParams(ui.dp(56), ui.dp(56)));
        LinearLayout names = new LinearLayout(this);
        names.setOrientation(LinearLayout.VERTICAL);
        names.setPadding(ui.dp(16), 0, 0, 0);
        TextView title = ui.text("carjack", 32, R.color.cj_on_surface);
        title.setTypeface(Typeface.create("sans-serif-medium", Typeface.NORMAL));
        names.addView(title);
        names.addView(ui.text("CarPlay from this phone", 15, R.color.cj_on_surface_variant));
        header.addView(names);
        page.addView(header);

        setup = ui.card(page, R.color.cj_tertiary_container);
        setup.setPadding(ui.dp(20), ui.dp(16), ui.dp(16), ui.dp(16));

        powerCard = ui.card(page, R.color.cj_primary_container);
        powerCard.setPadding(0, ui.dp(8), 0, ui.dp(8));
        power = new Switch(this);
        power.setOnClickListener(v -> setOn(power.isChecked()));
        status = ui.row(powerCard, getDrawable(R.drawable.ic_cj_cast), true, "Enabled", "",
                power, v -> setOn(!power.isChecked()));
        ((TextView) ((ViewGroup) status.getParent()).getChildAt(0)).setTextSize(20);
        actions = new LinearLayout(this);
        actions.setPadding(ui.dp(72), 0, ui.dp(16), ui.dp(8));
        retry = ui.button("Retry", true, v -> startService(
                new Intent(this, CarjackService.class).setAction(CarjackService.ACTION_RETRY)));
        actions.addView(retry);
        powerCard.addView(actions);

        page.addView(ui.heading("Connection"));
        settings = ui.card(page, R.color.cj_surface_container);
        settings.setPadding(0, ui.dp(4), 0, ui.dp(4));
        ((LinearLayout.LayoutParams) settings.getLayoutParams()).topMargin = 0;

        page.addView(ui.heading("Pairing"));
        LinearLayout pairing = ui.card(page, R.color.cj_surface_container);
        pairing.setPadding(0, ui.dp(4), 0, ui.dp(4));
        ((LinearLayout.LayoutParams) pairing.getLayoutParams()).topMargin = 0;
        ui.row(pairing, getDrawable(R.drawable.ic_cj_visibility), true,
                "Make this phone visible", "So a unit can find it to pair", null,
                v -> startActivity(new Intent(BluetoothAdapter.ACTION_REQUEST_DISCOVERABLE)
                        .putExtra(BluetoothAdapter.EXTRA_DISCOVERABLE_DURATION,
                                DISCOVERABLE_SECONDS)));
        ui.row(pairing, getDrawable(R.drawable.ic_cj_bluetooth), true, "Bluetooth settings",
                "Pair or forget units", null, v -> openBluetoothSettings());

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
        findUnits();
        showSettings();
        showSetup();
        autoSetUp();
    }

    private void showPower() {
        boolean on = BootReceiver.isOn(this);
        power.setChecked(on);
        status.setText(on ? Variant.setting(this, "status", "Waiting for the unit") : "Off");
        powerCard.setBackground(ui.shape(on ? R.color.cj_primary_container
                : R.color.cj_surface_container_high, 28));
        boolean showRetry = on && Variant.prefs(this).getBoolean("retryable", false);
        actions.setVisibility(showRetry ? View.VISIBLE : View.GONE);
    }

    /** Each a row showing its current value, opening its choices. */
    private void showSettings() {
        settings.removeAllViews();
        int unit = Math.max(0, indexOfIgnoreCase(unitAddresses,
                Variant.setting(this, "target", "")));
        ui.row(settings, getDrawable(R.drawable.ic_cj_unit), true, "Unit",
                unitNames.get(unit), ui.chevron(), v -> chooseUnit());

        String app = Choices.current(this);
        boolean glyph = app.equals(Previews.HOME) && Variant.homeApp(this) == null;
        ui.row(settings, Choices.icon(this, app), glyph, "On connect", Choices.name(this, app),
                ui.chevron(), v -> startActivity(new Intent(this, OnConnectActivity.class)));

        ui.row(settings, getDrawable(R.drawable.ic_cj_text_size), true, "Scale",
                SIZES[densityIndex()], ui.chevron(), v -> chooseSize());
    }

    private void chooseUnit() {
        int current = Math.max(0, indexOfIgnoreCase(unitAddresses,
                Variant.setting(this, "target", "")));
        new AlertDialog.Builder(this)
                .setTitle("Unit")
                .setSingleChoiceItems(unitNames.toArray(new String[0]), current, (d, i) -> {
                    Variant.prefs(this).edit().putString("target", unitAddresses.get(i)).apply();
                    d.dismiss();
                    showSettings();
                })
                .show();
    }

    private void chooseSize() {
        new AlertDialog.Builder(this)
                .setTitle("Scale on the unit")
                .setSingleChoiceItems(SIZES, densityIndex(), (d, i) -> {
                    Variant.prefs(this).edit().putString("density",
                            DENSITIES[i] == 0 ? "" : Integer.toString(DENSITIES[i])).apply();
                    d.dismiss();
                    showSettings();
                })
                .show();
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
                step("Bluetooth, Wi-Fi, notification and photo permissions", "Allow",
                        v -> requestPermissions(PERMISSIONS, REQUEST_PERMISSIONS));
            }
            if (!bluetoothOn()) {
                step("Bluetooth is off", "Turn on", v -> startActivity(
                        new Intent(BluetoothAdapter.ACTION_REQUEST_ENABLE)));
            }
        }
        if (permissionsGranted() && bluetoothOn() && unitAddresses.size() <= 1) {
            step("No CarPlay unit is paired yet", "Pair", v -> openBluetoothSettings());
        }
        if (setup.getChildCount() > 0) {
            LinearLayout heading = new LinearLayout(this);
            heading.setGravity(Gravity.CENTER_VERTICAL);
            heading.addView(ui.icon(R.drawable.ic_cj_shield, R.color.cj_on_tertiary_container,
                    20));
            TextView words = ui.text("Finish setting up", 16, R.color.cj_on_tertiary_container);
            words.setTypeface(Typeface.create("sans-serif-medium", Typeface.NORMAL));
            words.setPadding(ui.dp(10), 0, 0, 0);
            heading.addView(words);
            setup.addView(heading, 0);
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
    private void findUnits() {
        unitNames.clear();
        unitAddresses.clear();
        unitNames.add("Whichever connects");
        unitAddresses.add("");
        BluetoothAdapter adapter = getSystemService(BluetoothManager.class).getAdapter();
        if (bluetoothOn() && checkSelfPermission(Manifest.permission.BLUETOOTH_CONNECT)
                == PackageManager.PERMISSION_GRANTED) {
            for (BluetoothDevice device : adapter.getBondedDevices()) {
                if (Bluetooth.hostsIap(device.getUuids())) {
                    unitNames.add(device.getName() != null ? device.getName()
                            : device.getAddress());
                    unitAddresses.add(device.getAddress());
                }
            }
        }
    }

    private static int indexOfIgnoreCase(List<String> list, String value) {
        for (int i = 0; i < list.size(); i++) {
            if (list.get(i).equalsIgnoreCase(value)) {
                return i;
            }
        }
        return -1;
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

    private void step(String what, String action, View.OnClickListener onClick) {
        LinearLayout line = new LinearLayout(this);
        line.setGravity(Gravity.CENTER_VERTICAL);
        line.setPadding(0, ui.dp(12), 0, 0);
        TextView label = ui.text(what, 15, R.color.cj_on_tertiary_container);
        label.setPadding(0, 0, ui.dp(12), 0);
        line.addView(label, new LinearLayout.LayoutParams(0,
                LinearLayout.LayoutParams.WRAP_CONTENT, 1));
        line.addView(ui.button(action, true, onClick));
        setup.addView(line);
    }
}
