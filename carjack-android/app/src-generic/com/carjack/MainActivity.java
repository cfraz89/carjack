package com.carjack;

import android.Manifest;
import android.app.Activity;
import android.bluetooth.BluetoothAdapter;
import android.bluetooth.BluetoothDevice;
import android.bluetooth.BluetoothManager;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.graphics.Insets;
import android.os.Bundle;
import android.text.InputType;
import android.view.View;
import android.view.WindowInsets;
import android.widget.Button;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.RadioButton;
import android.widget.RadioGroup;
import android.widget.Switch;
import android.widget.TextView;

import rikka.shizuku.Shizuku;

/** The one-time setup a phone needs, then start and stop. */
public final class MainActivity extends Activity {
    private static final int REQUEST_PERMISSIONS = 1;
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
    private EditText app;
    private EditText density;
    private RadioGroup units;
    private Switch power;

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
        layout.addView(button("Let carjack use Shizuku", v -> {
            if (!pingShizuku()) {
                status.setText("Start Shizuku first (Shizuku app › Start via Wireless "
                        + "debugging), then try again.");
            } else if (Shizuku.checkSelfPermission() != PackageManager.PERMISSION_GRANTED) {
                Shizuku.requestPermission(REQUEST_SHIZUKU);
            }
        }));
        layout.addView(button("Make this phone visible to pair (5 minutes)", v -> {
            Intent intent = new Intent(BluetoothAdapter.ACTION_REQUEST_DISCOVERABLE);
            intent.putExtra(BluetoothAdapter.EXTRA_DISCOVERABLE_DURATION, DISCOVERABLE_SECONDS);
            startActivityForResult(intent, REQUEST_DISCOVERABLE);
        }));

        TextView appLabel = new TextView(this);
        appLabel.setText("\nThe app the bike shows (a package name; blank for a launcher):");
        layout.addView(appLabel);
        app = new EditText(this);
        app.setInputType(InputType.TYPE_CLASS_TEXT);
        app.setHint("com.google.android.apps.maps");
        app.setText(Variant.setting(this, "app", ""));
        layout.addView(app);
        TextView densityLabel = new TextView(this);
        densityLabel.setText("\nThe bike display's density in dpi; higher makes everything bigger "
                + "(blank for automatic, 200 on a 1080x600 unit):");
        layout.addView(densityLabel);
        density = new EditText(this);
        density.setInputType(InputType.TYPE_CLASS_NUMBER);
        density.setHint("200");
        density.setText(Variant.setting(this, "density", ""));
        layout.addView(density);
        layout.addView(button("Save", v -> {
            Variant.prefs(this).edit()
                    .putString("app", app.getText().toString().trim())
                    .putString("density", density.getText().toString().trim())
                    .apply();
            refresh();
        }));

        TextView unitLabel = new TextView(this);
        unitLabel.setText("\nWhich bike (paired CarPlay units):");
        layout.addView(unitLabel);
        units = new RadioGroup(this);
        layout.addView(units);

        power = new Switch(this);
        power.setText("carjack on (stays on, and starts with the phone)");
        power.setTextSize(18);
        power.setPadding(0, pad, 0, pad);
        power.setOnClickListener(v -> setOn(power.isChecked()));
        layout.addView(power, 1);

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

    private final Shizuku.OnRequestPermissionResultListener shizukuResult =
            (code, result) -> refresh();

    @Override
    protected void onDestroy() {
        Shizuku.removeRequestPermissionResultListener(shizukuResult);
        super.onDestroy();
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
        power.setChecked(BootReceiver.isOn(this));
        StringBuilder text = new StringBuilder("carjack: CarPlay from this phone\n\n");
        text.append(permissionsGranted() ? "✓" : "✗").append(" Permissions\n");
        BluetoothAdapter adapter = getSystemService(BluetoothManager.class).getAdapter();
        text.append(adapter != null && adapter.isEnabled() ? "✓" : "✗").append(" Bluetooth on\n");
        text.append(!pingShizuku() ? "✗ Shizuku isn't running\n"
                : ShizukuDisplay.available() ? "✓ Shizuku\n"
                : "✗ Shizuku hasn't let carjack in\n");
        text.append("\nTurn carjack on, then pair with the bike once, from its menu or this "
                + "phone's Bluetooth settings. From then on carjack waits for the bike and gives "
                + "it a display of its own size; Shizuku has to be running by the time it "
                + "connects.");
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
        refresh();
    }
}
