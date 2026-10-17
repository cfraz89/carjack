package com.carjack;

import android.Manifest;
import android.app.Activity;
import android.bluetooth.BluetoothAdapter;
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
import android.widget.TextView;

/** The one-time setup a phone needs, then start and stop. */
public final class MainActivity extends Activity {
    private static final int REQUEST_PERMISSIONS = 1;
    private static final int REQUEST_CAPTURE = 2;
    private static final int REQUEST_DISCOVERABLE = 3;
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
        layout.addView(button("Save", v -> {
            Variant.prefs(this).edit()
                    .putString("bluetooth_address", address.getText().toString().trim())
                    .apply();
            refresh();
        }));

        layout.addView(button("Start", v -> {
            // A connected-device foreground service can't start before Bluetooth is granted.
            if (!permissionsGranted()) {
                requestPermissions(PERMISSIONS, REQUEST_PERMISSIONS);
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

    private void refresh() {
        StringBuilder text = new StringBuilder("carjack: CarPlay from this phone\n\n");
        text.append(permissionsGranted() ? "✓" : "✗").append(" Permissions\n");
        BluetoothAdapter adapter = getSystemService(BluetoothManager.class).getAdapter();
        text.append(adapter != null && adapter.isEnabled() ? "✓" : "✗").append(" Bluetooth on\n");
        text.append(CarjackAccessibility.instance != null ? "✓" : "✗")
                .append(" Touch control\n");
        text.append(Variant.setting(this, "bluetooth_address", "").isEmpty() ? "✗" : "✓")
                .append(" Bluetooth address\n\n")
                .append("Pair with the bike once, from its menu or this phone's Bluetooth "
                        + "settings. Then Start, before the bike connects: carjack waits for "
                        + "it and mirrors this screen to it; hold the phone sideways for the "
                        + "best fit. If the bike was already connected, turn Bluetooth off and "
                        + "on again.");
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
