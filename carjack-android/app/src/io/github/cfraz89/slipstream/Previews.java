package io.github.cfraz89.slipstream;

import android.content.Context;
import android.graphics.Bitmap;
import android.util.Log;

import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;

/**
 * The last picture each on-connect choice put on the unit, for the picker's thumbnails. Keyed by
 * the "app" setting: the launcher, the phone's home app, or a package.
 */
final class Previews {
    private static final String TAG = "carjack";
    static final String HOME = "@home";

    static File file(Context context, String app) {
        String key = app.isEmpty() ? "launcher" : app.equals(HOME) ? "home"
                : app.replaceAll("[^A-Za-z0-9._]", "_");
        return new File(new File(context.getFilesDir(), "previews"), key + ".png");
    }

    /** Off the caller's thread: compressing a PNG takes a while. */
    static void save(Context context, String app, Bitmap bitmap) {
        new Thread(() -> {
            File file = file(context, app);
            file.getParentFile().mkdirs();
            File partial = new File(file.getPath() + ".part");
            try (FileOutputStream out = new FileOutputStream(partial)) {
                bitmap.compress(Bitmap.CompressFormat.PNG, 100, out);
            } catch (IOException e) {
                Log.w(TAG, "No preview saved", e);
                return;
            } finally {
                bitmap.recycle();
            }
            partial.renameTo(file);
        }, "carjack-preview").start();
    }

    private Previews() {}
}
