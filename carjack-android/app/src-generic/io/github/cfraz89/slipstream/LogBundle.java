package io.github.cfraz89.slipstream;

import android.content.Context;
import android.content.Intent;
import android.os.Build;
import android.util.Log;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.util.Map;
import java.util.zip.ZipEntry;
import java.util.zip.ZipOutputStream;

/**
 * Everything a report needs, zipped for sending: Slipstream's own log, the phone's system log
 * (which only Shizuku's shell can reach), and what phone and settings it ran on.
 */
final class LogBundle {
    private static final String TAG = "carjack";
    /** Where reports go; the share sheet still lets the sender pick another. */
    static final String REPORT_EMAIL = "cfraz89@gmail.com";

    static File dir(Context context) {
        return new File(context.getCacheDir(), "logs");
    }

    /**
     * Builds a bundle and sends it through {@link Reports}, returning null; or, with reports off
     * or a bundle too big for them, returns the share sheet to send it with. Blocks: not on the
     * UI thread.
     */
    static Intent send(Context context, String description, boolean email) throws IOException {
        File out = build(context);
        if (!email && Reports.on() && out.length() <= Reports.MAX_ATTACHMENT) {
            Reports.sendLogs(out, description);
            return null;
        }
        Intent send = new Intent(Intent.ACTION_SEND)
                .setType("application/zip")
                .putExtra(Intent.EXTRA_STREAM, LogsProvider.uriFor(out))
                .putExtra(Intent.EXTRA_EMAIL, new String[] {REPORT_EMAIL})
                .putExtra(Intent.EXTRA_SUBJECT, "Slipstream logs")
                .putExtra(Intent.EXTRA_TEXT, "What happened, and roughly when:\n\n" + description)
                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
        return Intent.createChooser(send, "Send logs");
    }

    private static File build(Context context) throws IOException {
        File dir = dir(context);
        dir.mkdirs();
        // One bundle at a time: they're tens of megabytes.
        File[] old = dir.listFiles();
        if (old != null) {
            for (File f : old) {
                f.delete();
            }
        }
        File external = context.getExternalFilesDir(null);
        File system = external == null ? null : new File(external, "system");
        if (system != null) {
            system.mkdirs();
            File[] stale = system.listFiles();
            if (stale != null) {
                for (File f : stale) {
                    f.delete();
                }
            }
            ShizukuDisplay.copySystemLog(system.getPath());
        }

        String stamp = new java.text.SimpleDateFormat("yyyyMMdd-HHmmss", java.util.Locale.ROOT)
                .format(new java.util.Date());
        File out = new File(dir, "slipstream-logs-" + stamp + ".zip");
        try (ZipOutputStream zip = new ZipOutputStream(new FileOutputStream(out))) {
            add(zip, "info.txt", info(context).getBytes(StandardCharsets.UTF_8));
            if (external != null) {
                addLogs(zip, external, Variant.LOG_FILE);
            }
            if (system != null) {
                addLogs(zip, system, "slipstream-system.log");
            }
        }
        Log.i(TAG, "Log bundle of " + out.length() + " bytes");
        return out;
    }

    private static void addLogs(ZipOutputStream zip, File dir, String base) throws IOException {
        File[] files = dir.listFiles((d, name) -> name.startsWith(base));
        if (files == null) {
            return;
        }
        // Not closed: that would close the zip.
        java.io.Writer out = new java.io.OutputStreamWriter(zip, StandardCharsets.UTF_8);
        for (File file : files) {
            zip.putNextEntry(new ZipEntry(file.getName()));
            try (java.io.BufferedReader in = new java.io.BufferedReader(new java.io.InputStreamReader(
                    new FileInputStream(file), StandardCharsets.UTF_8))) {
                String line;
                while ((line = in.readLine()) != null) {
                    if (!secret(line)) {
                        out.write(line);
                        out.write('\n');
                    }
                }
            }
            out.flush();
            zip.closeEntry();
        }
    }

    /** Lines that may carry the unit's WiFi password. */
    private static boolean secret(String line) {
        return line.contains("passphrase=") || line.contains("connect-network");
    }

    private static void add(ZipOutputStream zip, String name, byte[] bytes) throws IOException {
        zip.putNextEntry(new ZipEntry(name));
        zip.write(bytes);
        zip.closeEntry();
    }

    private static String info(Context context) {
        StringBuilder info = new StringBuilder();
        try {
            info.append("Slipstream ").append(context.getPackageManager()
                    .getPackageInfo(context.getPackageName(), 0).versionName).append('\n');
        } catch (Exception ignored) {
        }
        info.append(Build.MANUFACTURER).append(' ').append(Build.MODEL)
                .append(", Android ").append(Build.VERSION.RELEASE)
                .append(" (").append(Build.DISPLAY).append(")\n")
                .append("Shizuku: ").append(ShizukuDisplay.available() ? "yes" : "no").append("\n\n")
                .append("Settings:\n");
        for (Map.Entry<String, ?> setting : Variant.prefs(context).getAll().entrySet()) {
            info.append("  ").append(setting.getKey()).append(" = ")
                    .append(setting.getValue()).append('\n');
        }
        return info.toString();
    }

    private LogBundle() {}
}
