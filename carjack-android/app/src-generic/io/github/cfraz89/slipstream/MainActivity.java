package io.github.cfraz89.slipstream;

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
    private static final String TAILSCALE_PACKAGE = "com.tailscale.ipn";

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
    private LinearLayout pairCard;
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
        logo.setImageResource(R.drawable.ic_sl_app);
        header.addView(logo, new LinearLayout.LayoutParams(ui.dp(56), ui.dp(56)));
        LinearLayout names = new LinearLayout(this);
        names.setOrientation(LinearLayout.VERTICAL);
        names.setPadding(ui.dp(16), 0, 0, 0);
        // Yellowtail (Apache 2.0, res/font): a fast script, shaded pink into blue like a trail.
        TextView title = ui.text("Slipstream", 40, R.color.sl_tertiary);
        title.setTypeface(getResources().getFont(R.font.yellowtail));
        float titleWidth = title.getPaint().measureText("Slipstream");
        title.getPaint().setShader(new android.graphics.LinearGradient(0, 0, titleWidth, 0,
                ui.color(R.color.sl_tertiary), ui.color(R.color.sl_primary),
                android.graphics.Shader.TileMode.CLAMP));
        names.addView(title);
        header.addView(names);
        page.addView(header);

        setup = ui.card(page, R.color.sl_tertiary_container);
        setup.setPadding(ui.dp(20), ui.dp(16), ui.dp(16), ui.dp(16));

        pairCard = ui.card(page, R.color.sl_primary_container);
        pairCard.setPadding(ui.dp(20), ui.dp(16), ui.dp(16), ui.dp(16));
        buildPairCard();

        powerCard = ui.card(page, R.color.sl_primary_container);
        powerCard.setPadding(0, ui.dp(8), 0, ui.dp(8));
        power = new Switch(this);
        int[][] states = {{android.R.attr.state_checked}, {}};
        power.setThumbTintList(new android.content.res.ColorStateList(states, new int[] {
                ui.color(R.color.sl_on_tertiary), ui.color(R.color.sl_outline)}));
        power.setTrackTintList(new android.content.res.ColorStateList(states, new int[] {
                ui.color(R.color.sl_tertiary), ui.color(R.color.sl_surface_container_highest)}));
        power.setOnClickListener(v -> setOn(power.isChecked()));
        status = ui.row(powerCard, getDrawable(R.drawable.ic_sl_cast), true, "Enabled", "",
                power, v -> setOn(!power.isChecked()));
        ((TextView) ((ViewGroup) status.getParent()).getChildAt(0)).setTextSize(20);
        actions = new LinearLayout(this);
        actions.setPadding(ui.dp(72), 0, ui.dp(16), ui.dp(8));
        retry = ui.button("Retry", true, v -> startService(
                new Intent(this, SlipstreamService.class).setAction(SlipstreamService.ACTION_RETRY)));
        actions.addView(retry);
        powerCard.addView(actions);

        page.addView(ui.heading("Connection"));
        settings = ui.card(page, R.color.sl_surface_container);
        settings.setPadding(0, ui.dp(4), 0, ui.dp(4));
        ((LinearLayout.LayoutParams) settings.getLayoutParams()).topMargin = 0;

        page.addView(ui.heading("Pairing"));
        LinearLayout pairing = ui.card(page, R.color.sl_surface_container);
        pairing.setPadding(0, ui.dp(4), 0, ui.dp(4));
        ((LinearLayout.LayoutParams) pairing.getLayoutParams()).topMargin = 0;
        ui.row(pairing, getDrawable(R.drawable.ic_sl_visibility), true,
                "Make this phone visible", "So a unit can find it to pair", null,
                v -> startActivity(new Intent(BluetoothAdapter.ACTION_REQUEST_DISCOVERABLE)
                        .putExtra(BluetoothAdapter.EXTRA_DISCOVERABLE_DURATION,
                                DISCOVERABLE_SECONDS)));
        ui.row(pairing, getDrawable(R.drawable.ic_sl_bluetooth), true, "Bluetooth settings",
                "Pair or forget units", null, v -> openBluetoothSettings());

        page.addView(ui.heading("Help"));
        LinearLayout help = ui.card(page, R.color.sl_surface_container);
        help.setPadding(0, ui.dp(4), 0, ui.dp(4));
        ((LinearLayout.LayoutParams) help.getLayoutParams()).topMargin = 0;
        ui.row(help, getDrawable(R.drawable.ic_sl_share), true, "Send logs",
                "For a report of something going wrong", null, v -> confirmSendLogs());

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

    /** First-time pairing, step by step: the unit searches, the phone is found, codes match. */
    private void buildPairCard() {
        LinearLayout heading = new LinearLayout(this);
        heading.setGravity(Gravity.CENTER_VERTICAL);
        heading.addView(ui.icon(R.drawable.ic_sl_bluetooth, R.color.sl_on_primary_container, 20));
        TextView words = ui.text("Pair your head unit", 16, R.color.sl_on_primary_container);
        words.setTypeface(Typeface.create("sans-serif-medium", Typeface.NORMAL));
        words.setPadding(ui.dp(10), 0, 0, 0);
        heading.addView(words);
        pairCard.addView(heading);
        String[] steps = {
            "On the unit, open its phone or Bluetooth menu and add a device",
            "Make this phone visible, so the unit can find it",
            "Pick this phone on the unit, and confirm the code on both",
        };
        for (int i = 0; i < steps.length; i++) {
            LinearLayout line = new LinearLayout(this);
            line.setPadding(0, ui.dp(10), 0, 0);
            TextView number = ui.text(Integer.toString(i + 1), 13, R.color.sl_on_tertiary);
            number.setGravity(Gravity.CENTER);
            number.setBackground(ui.shape(R.color.sl_tertiary, 11));
            line.addView(number, new LinearLayout.LayoutParams(ui.dp(22), ui.dp(22)));
            TextView step = ui.text(steps[i], 15, R.color.sl_on_primary_container);
            step.setPadding(ui.dp(12), 0, 0, 0);
            line.addView(step);
            pairCard.addView(line);
        }
        LinearLayout buttons = new LinearLayout(this);
        buttons.setPadding(0, ui.dp(14), 0, 0);
        buttons.addView(ui.button("Make visible", true, v -> startActivity(
                new Intent(BluetoothAdapter.ACTION_REQUEST_DISCOVERABLE)
                        .putExtra(BluetoothAdapter.EXTRA_DISCOVERABLE_DURATION,
                                DISCOVERABLE_SECONDS))));
        buttons.addView(ui.button("Bluetooth settings", false, v -> openBluetoothSettings()));
        pairCard.addView(buttons);
    }

    private void showPower() {
        boolean on = BootReceiver.isOn(this);
        power.setChecked(on);
        String waiting = Variant.prefs(this).getBoolean("linked", false) ? "Waiting for the unit"
                : "Waiting for the unit: turn it on, and it connects by itself";
        String current = Variant.setting(this, "status", "Waiting for the unit");
        status.setText(!on ? "Off" : current.equals("Waiting for the unit") ? waiting : current);
        powerCard.setBackground(ui.shape(on ? R.color.sl_primary_container
                : R.color.sl_surface_container_high, 28));
        boolean showRetry = on && Variant.prefs(this).getBoolean("retryable", false);
        actions.setVisibility(showRetry ? View.VISIBLE : View.GONE);
    }

    /** Each a row showing its current value, opening its choices. */
    private void showSettings() {
        settings.removeAllViews();
        int unit = Math.max(0, indexOfIgnoreCase(unitAddresses,
                Variant.setting(this, "target", "")));
        ui.row(settings, getDrawable(R.drawable.ic_sl_unit), true, "Unit",
                unitNames.get(unit), ui.chevron(), v -> chooseUnit());

        String app = Choices.current(this);
        boolean glyph = app.equals(Previews.HOME) && Variant.homeApp(this) == null;
        ui.row(settings, Choices.icon(this, app), glyph, "On connect", Choices.name(this, app),
                ui.chevron(), v -> startActivity(new Intent(this, OnConnectActivity.class)));

        ui.row(settings, getDrawable(R.drawable.ic_sl_text_size), true, "Scale",
                SIZES[densityIndex()], ui.chevron(), v -> chooseSize());

        Switch dark = new Switch(this);
        dark.setChecked(Variant.keepsPanel(this));
        dark.setClickable(false);
        ui.row(settings, getDrawable(R.drawable.ic_sl_cast), true,
                "Dark screen keeps the unit going (experimental)",
                "During a session the phone's screen goes dark instead of the phone sleeping, so"
                        + " the unit stays usable. Power button or a touch brings it back."
                        + " Takes effect next session",
                dark, v -> {
                    dark.setChecked(!dark.isChecked());
                    Variant.prefs(this).edit()
                            .putBoolean("keep_panel", dark.isChecked()).apply();
                });

        Switch navBar = new Switch(this);
        navBar.setChecked(Boolean.parseBoolean(Variant.setting(this, "nav_bar", "false")));
        navBar.setClickable(false);
        ui.row(settings, getDrawable(R.drawable.ic_sl_home), true, "Back and home bar",
                "A slim bar along the bottom of the unit, for when its switchgear can't go back"
                        + " or home. Takes effect next session",
                navBar, v -> {
                    navBar.setChecked(!navBar.isChecked());
                    Variant.prefs(this).edit()
                            .putString("nav_bar", Boolean.toString(navBar.isChecked())).apply();
                });

        Switch unlocked = new Switch(this);
        unlocked.setChecked(Variant.holdsUnlock(this));
        unlocked.setClickable(false);
        ui.row(settings, getDrawable(R.drawable.ic_sl_lock_open), true,
                "Stay unlocked while connected",
                "The screen can go off without locking, so the unit carries on. For the power"
                        + " button too, turn off \"Power button instantly locks\" in the screen"
                        + " lock's settings",
                unlocked, v -> {
                    unlocked.setChecked(!unlocked.isChecked());
                    Variant.prefs(this).edit()
                            .putBoolean("hold_unlock", unlocked.isChecked()).apply();
                    Variant.applyLockHold(this);
                });
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

    /** The logs say more than their name suggests, so the sender knows before they go. */
    private void confirmSendLogs() {
        new AlertDialog.Builder(this)
                .setTitle("Send logs?")
                .setMessage("They cover the last few days of Slipstream and what the phone did"
                        + " around it, including unit names, Bluetooth addresses and the unit's"
                        + " WiFi password. Send them only to whoever's looking into a problem.")
                .setPositiveButton("Send", (d, w) -> sendLogs())
                .setNegativeButton("Cancel", null)
                .show();
    }

    private void sendLogs() {
        android.widget.Toast.makeText(this, "Gathering logs…", android.widget.Toast.LENGTH_SHORT)
                .show();
        new Thread(() -> {
            try {
                Intent share = LogBundle.share(this);
                runOnUiThread(() -> startActivity(share));
            } catch (java.io.IOException e) {
                runOnUiThread(() -> android.widget.Toast.makeText(this,
                        "Couldn't gather the logs: " + e.getMessage(),
                        android.widget.Toast.LENGTH_LONG).show());
            }
        }, "slipstream-logs").start();
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
            step("Let Slipstream use Shizuku", "Allow",
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
        // Pairing has a card of its own, once there's Bluetooth to pair with.
        pairCard.setVisibility(permissionsGranted() && bluetoothOn() && unitAddresses.size() <= 1
                ? View.VISIBLE : View.GONE);
        // An app inside a VPN can't reach the unit's Wi-Fi: the stream times out.
        if (inVpn()) {
            // Excluding an app is the VPN app's own setting; Android's VPN page only has on/off.
            Intent tailscale = getPackageManager().getLaunchIntentForPackage(TAILSCALE_PACKAGE);
            if (tailscale != null) {
                step("Tailscale blocks the unit's Wi-Fi. In Tailscale's settings, exclude"
                        + " Slipstream under split tunneling, or turn Tailscale off",
                        "Tailscale", v -> startActivity(tailscale));
            } else {
                step("A VPN is on, which blocks the unit's Wi-Fi. Exclude Slipstream in the VPN"
                        + " app's settings, or turn it off", "VPN", v -> startActivity(
                                new Intent(android.provider.Settings.ACTION_VPN_SETTINGS)));
            }
        }
        if (setup.getChildCount() > 0) {
            LinearLayout heading = new LinearLayout(this);
            heading.setGravity(Gravity.CENTER_VERTICAL);
            heading.addView(ui.icon(R.drawable.ic_sl_shield, R.color.sl_on_tertiary_container,
                    20));
            TextView words = ui.text("Finish setting up", 16, R.color.sl_on_tertiary_container);
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
        }, "slipstream-setup").start();
    }

    private void setOn(boolean on) {
        // A connected-device foreground service can't start before Bluetooth is granted.
        if (on && !permissionsGranted()) {
            requestPermissions(PERMISSIONS, REQUEST_PERMISSIONS);
            on = false;
        }
        BootReceiver.setOn(this, on);
        Intent service = new Intent(this, SlipstreamService.class);
        if (on) {
            startForegroundService(service);
        } else {
            stopService(service);
        }
        showPower();
    }

    /** Automatic, then each paired head unit. */
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

    /** This app's traffic goes through a VPN. */
    private boolean inVpn() {
        android.net.ConnectivityManager cm =
                getSystemService(android.net.ConnectivityManager.class);
        android.net.NetworkCapabilities caps = cm.getNetworkCapabilities(cm.getActiveNetwork());
        return caps != null && caps.hasTransport(android.net.NetworkCapabilities.TRANSPORT_VPN);
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
        TextView label = ui.text(what, 15, R.color.sl_on_tertiary_container);
        label.setPadding(0, 0, ui.dp(12), 0);
        line.addView(label, new LinearLayout.LayoutParams(0,
                LinearLayout.LayoutParams.WRAP_CONTENT, 1));
        line.addView(ui.button(action, true, onClick));
        setup.addView(line);
    }
}
