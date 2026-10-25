package io.github.cfraz89.slipstream

import android.content.Context
import android.graphics.Bitmap
import android.util.Log
import java.io.File
import java.io.FileOutputStream
import java.io.IOException

/**
 * The last picture each on-connect choice put on the unit, for the picker's thumbnails. Keyed by
 * the "app" setting: the launcher, the phone's home app, or a package.
 */
object Previews {
    private const val TAG = "carjack"
    const val HOME = "@home"

    fun file(context: Context, app: String): File {
        val key = when {
            app.isEmpty() -> "launcher"
            app == HOME -> "home"
            else -> app.replace(Regex("[^A-Za-z0-9._]"), "_")
        }
        return File(File(context.filesDir, "previews"), "$key.png")
    }

    /** Off the caller's thread: compressing a PNG takes a while. */
    fun save(context: Context, app: String, bitmap: Bitmap) {
        Thread({
            val file = file(context, app)
            file.parentFile?.mkdirs()
            val partial = File(file.path + ".part")
            try {
                FileOutputStream(partial).use { bitmap.compress(Bitmap.CompressFormat.PNG, 100, it) }
            } catch (e: IOException) {
                Log.w(TAG, "No preview saved", e)
                return@Thread
            } finally {
                bitmap.recycle()
            }
            partial.renameTo(file)
        }, "carjack-preview").start()
    }
}
