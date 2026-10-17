package com.carjack;

import android.Manifest;
import android.app.Activity;
import android.bluetooth.BluetoothAdapter;
import android.bluetooth.BluetoothDevice;
import android.bluetooth.BluetoothManager;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.graphics.Insets;
import android.media.projection.MediaProjectionConfig;
import android.media.projection.MediaProjectionManager;
import android.os.Bundle;
import android.provider.Settings;
import android.text.InputType;
import android.view.View;
import android.view.WindowInsets;
import android.widget.Button;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.RadioButton;
import android.widget.RadioGroup;
import android.widget.TextView;

import rikka.shizuku.Shizuku;

/** The one-time setup a phone needs, then start and stop. */
public final class MainActivity extends Activity {
    private static final int REQUEST_PERMISSIONS = 1;
    private static final int REQUEST_CAPTURE = 2;
    private static final int REQUEST_DISCOVERABLE = 3;
    private static final int REQUEST_SHIZUKU = 4;
    private static final int DISCOVERABLE_SECONDS = 300;

    private static final String[] PERMISSIONS = {
        Manifest.permission.BLUETOOTH_CONNECT,
        Manifest.permission.BLUETOOTH_SCAN,
        Manifest.permission.BLUETOOTH_ADVERTISE,
        Manifest.permission.NEARBY_WIFI_DEVICES,
        Manifest.permission.POST_NOTIFICATIONS,
    };

    private TextView status;
    private EditText address;
    private EditText app;
    private RadioGroup units;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        LinearLayout layout = new LinearLayout(this);
        layout.setOrientation(LinearLayout.VERTICAL);
        int pad = (int) (16 * getResources().getDisplayMetrics().density);
        layout.setPadding(pad, pad, pad, pad);

        status = new TextView(this);
        layout.addView(status);
        layout.addView(button("Allow Bluetooth, Wi-Fi and notifications",
                v -> requestPermissions(PERMISSIONS, REQUEST_PERMISSIONS)));
        layout.addView(button("Make this phone visible to pair (5 minutes)", v -> {
            Intent intent = new Intent(BluetoothAdapter.ACTION_REQUEST_DISCOVERABLE);
            intent.putExtra(BluetoothAdapter.EXTRA_DISCOVERABLE_DURATION, DISCOVERABLE_SECONDS);
            startActivityForResult(intent, REQUEST_DISCOVERABLE);
        }));
        layout.addView(button("Turn on touch control (Accessibility › carjack)",
                v -> startActivity(new Intent(Settings.ACTION_ACCESSIBILITY_SETTINGS))));

        TextView label = new TextView(this);
        label.setText("\nThis phone's Bluetooth address (Settings › About phone), which the bike "
                + "is told; apps can't read it themselves:");
        layout.addView(label);
        address = new EditText(this);
        address.setInputType(InputType.TYPE_CLASS_TEXT
                | InputType.TYPE_TEXT_FLAG_CAP_CHARACTERS);
        address.setHint("AA:BB:CC:DD:EE:FF");
        address.setText(Variant.setting(this, "bluetooth_address", ""));
        layout.addView(address);
        TextView appLabel = new TextView(this);
        appLabel.setText("\nWith Shizuku, the app the bike shows (a package name; blank for a "
                + "launcher):");
        layout.addView(appLabel);
        app = new EditText(this);
        app.setInputType(InputType.TYPE_CLASS_TEXT);
        app.setHint("com.google.android.apps.maps");
        app.setText(Variant.setting(this, "app", ""));
        layout.addView(app);
        layout.addView(button("Save", v -> {
            Variant.prefs(this).edit()
                    .putString("bluetooth_address", address.getText().toString().trim())
                    .putString("app", app.getText().toString().trim())
                    .apply();
            refresh();
        }));

        TextView unitLabel = new TextView(this);
        unitLabel.setText("\nWhich bike (paired CarPlay units):");
        layout.addView(unitLabel);
        units = new RadioGroup(this);
        layout.addView(units);

        layout.addView(button("Use Shizuku: a display at the bike's resolution", v -> {
            if (!pingShizuku()) {
                status.setText("Start Shizuku first (Shizuku app › Start via Wireless "
                        + "debugging), then try again.");
                return;
            }
            if (Shizuku.checkSelfPermission() == PackageManager.PERMISSION_GRANTED) {
                setDisplay("shizuku");
            } else {
                Shizuku.requestPermission(REQUEST_SHIZUKU);
            }
        }));
        layout.addView(button("Mirror this phone's screen instead",
                v -> setDisplay("mirror")));

        layout.addView(button("Start", v -> {
            // A connected-device foreground service can't start before Bluetooth is granted.
            if (!permissionsGranted()) {
                requestPermissions(PERMISSIONS, REQUEST_PERMISSIONS);
                return;
            }
            if (Variant.usesShizuku(this)) {
                startForegroundService(new Intent(this, CarjackService.class));
                return;
            }
            // The whole screen: the bike mirrors whatever app is in front.
            startActivityForResult(
                    getSystemService(MediaProjectionManager.class).createScreenCaptureIntent(
                            MediaProjectionConfig.createConfigForDefaultDisplay()),
                    REQUEST_CAPTURE);
        }));
        layout.addView(button("Stop",
                v -> stopService(new Intent(this, CarjackService.class))));

        ScrollView scroll = new ScrollView(this);
        scroll.addView(layout);
        // Drawn edge to edge, so the bars' insets are ours to keep clear of.
        scroll.setOnApplyWindowInsetsListener((v, insets) -> {
            Insets bars = insets.getInsets(WindowInsets.Type.systemBars());
            v.setPadding(bars.left, bars.top, bars.right, bars.bottom);
            return WindowInsets.CONSUMED;
        });
        setContentView(scroll);
        Shizuku.addRequestPermissionResultListener(shizukuResult);
    }

    private final Shizuku.OnRequestPermissionResultListener shizukuResult = (code, result) -> {
        if (code == REQUEST_SHIZUKU && result == PackageManager.PERMISSION_GRANTED) {
            setDisplay("shizuku");
        }
    };

    @Override
    protected void onDestroy() {
        Shizuku.removeRequestPermissionResultListener(shizukuResult);
        super.onDestroy();
    }

    private void setDisplay(String mode) {
        Variant.prefs(this).edit().putString("display", mode).apply();
        refresh();
    }

    private static boolean pingShizuku() {
        try {
            return Shizuku.pingBinder();
        } catch (RuntimeException e) {
            return false;
        }
    }

    private Button button(String text, View.OnClickListener listener) {
        Button button = new Button(this);
        button.setText(text);
        button.setAllCaps(false);
        button.setOnClickListener(listener);
        return button;
    }

    @Override
    protected void onResume() {
        super.onResume();
        refresh();
    }

    /** Automatic, then each paired unit; the saved choice checked. */
    private void listUnits() {
        units.setOnCheckedChangeListener(null);
        units.removeAllViews();
        String target = Variant.setting(this, "target", "");
        units.addView(unit("Automatic: whichever is connected", ""));
        BluetoothAdapter adapter = getSystemService(BluetoothManager.class).getAdapter();
        if (adapter != null && adapter.isEnabled()
                && checkSelfPermission(Manifest.permission.BLUETOOTH_CONNECT)
                        == PackageManager.PERMISSION_GRANTED) {
            for (BluetoothDevice device : adapter.getBondedDevices()) {
                if (Bluetooth.hostsIap(device.getUuids())) {
                    units.addView(unit(device.getName() + "  " + device.getAddress(),
                            device.getAddress()));
                }
            }
        }
        for (int i = 0; i < units.getChildCount(); i++) {
            RadioButton button = (RadioButton) units.getChildAt(i);
            if (target.equalsIgnoreCase((String) button.getTag())) {
                units.check(button.getId());
            }
        }
        units.setOnCheckedChangeListener((group, id) -> {
            RadioButton chosen = group.findViewById(id);
            if (chosen != null) {
                Variant.prefs(this).edit().putString("target", (String) chosen.getTag()).apply();
            }
        });
    }

    private RadioButton unit(String label, String address) {
        RadioButton button = new RadioButton(this);
        button.setId(View.generateViewId());
        button.setText(label);
        button.setTag(address);
        return button;
    }

    private void refresh() {
        listUnits();
        StringBuilder text = new StringBuilder("carjack: CarPlay from this phone\n\n");
        text.append(permissionsGranted() ? "✓" : "✗").append(" Permissions\n");
        BluetoothAdapter adapter = getSystemService(BluetoothManager.class).getAdapter();
        text.append(adapter != null && adapter.isEnabled() ? "✓" : "✗").append(" Bluetooth on\n");
        text.append(CarjackAccessibility.instance != null ? "✓" : "✗")
                .append(" Touch control\n");
        text.append(Variant.setting(this, "bluetooth_address", "").isEmpty() ? "✗" : "✓")
                .append(" Bluetooth address\n");
        boolean shizuku = "shizuku".equals(Variant.setting(this, "display", "mirror"));
        text.append("Display: ").append(!shizuku ? "mirror of this screen"
                : Variant.usesShizuku(this) ? "Shizuku, at the bike's resolution"
                : "Shizuku, which isn't running (mirroring until it is)").append("\n\n");
        text.append("Pair with the bike once, from its menu or this phone's Bluetooth "
                + "settings. Then Start, before the bike connects: carjack waits for it and "
                + (shizuku ? "gives it a display of its own size."
                        : "mirrors this screen to it; hold the phone sideways for the best fit.")
                + " If the bike was already connected, turn Bluetooth off and on again.");
        status.setText(text);
    }

    private boolean permissionsGranted() {
        for (String permission : PERMISSIONS) {
            if (checkSelfPermission(permission) != PackageManager.PERMISSION_GRANTED) {
                return false;
            }
        }
        return true;
    }

    @Override
    public void onRequestPermissionsResult(int requestCode, String[] permissions, int[] results) {
        refresh();
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        if (requestCode == REQUEST_CAPTURE && resultCode == RESULT_OK && data != null) {
            Intent service = new Intent(this, CarjackService.class)
                    .putExtra(Variant.EXTRA_RESULT_CODE, resultCode)
                    .putExtra(Variant.EXTRA_RESULT_DATA, data);
            startForegroundService(service);
        }
        refresh();
    }
}
