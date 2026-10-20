package io.github.cfraz89.slipstream.launcher

import android.app.WallpaperManager
import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.drawable.BitmapDrawable
import android.graphics.drawable.Drawable
import android.net.Uri
import android.util.Log
import java.io.File
import kotlin.math.max

/** The launcher's own wallpaper choice: the phone's, the system's built-in one, or a photo. */
object Wallpapers {
    private const val TAG = "SlipstreamWallpaper"
    private const val KEY = "wallpaper"
    private const val PHOTO_FILE = "launcher_wallpaper"

    const val PHONE = "phone"
    const val BUILT_IN = "builtin"
    const val PHOTO = "photo"

    /**
     * Set by the app: the phone's wallpaper as the system would frame it for a display of this
     * size, or null where it can't say. Called off the main thread.
     */
    @JvmStatic
    var phoneFramed: ((Int, Int) -> Bitmap?)? = null

    fun choice(context: Context): String = Prefs.get(context).getString(KEY, PHONE) ?: PHONE

    /** Changes whenever what [load] gives would: the choice, or a newly picked photo. */
    fun stamp(context: Context): String =
        "${choice(context)} ${File(context.filesDir, PHOTO_FILE).lastModified()}"

    fun choose(context: Context, value: String) {
        Prefs.get(context).edit().putString(KEY, value).apply()
    }

    /** Copies the picked photo in, since the picker's grant doesn't outlive the activity. Blocks. */
    fun savePhoto(context: Context, uri: Uri): Boolean = try {
        context.contentResolver.openInputStream(uri).use { input ->
            File(context.filesDir, PHOTO_FILE).outputStream().use { input!!.copyTo(it) }
        }
        choose(context, PHOTO)
        true
    } catch (e: Exception) {
        Log.w(TAG, "Couldn't keep the chosen photo", e)
        false
    }

    /** The chosen wallpaper for a [width]x[height] screen. Blocks: run it off the main thread. */
    fun load(context: Context, width: Int, height: Int): Drawable {
        val manager = WallpaperManager.getInstance(context)
        val bitmap = when (choice(context)) {
            PHOTO -> decodeFor(File(context.filesDir, PHOTO_FILE), width, height)
            BUILT_IN -> null
            else -> phoneFramed?.invoke(width, height) ?: phoneCropped(manager)
        }
        return bitmap?.let { BitmapDrawable(context.resources, it) } ?: manager.builtInDrawable
    }

    /** The phone's own crop of its wallpaper, where the app may read it (READ_MEDIA_IMAGES). */
    private fun phoneCropped(manager: WallpaperManager): Bitmap? = try {
        (manager.drawable as? BitmapDrawable)?.bitmap
    } catch (e: SecurityException) {
        null
    }

    private fun decodeFor(file: File, width: Int, height: Int): Bitmap? {
        if (!file.exists()) return null
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeFile(file.path, bounds)
        var sample = 1
        while (bounds.outWidth / (sample * 2) >= width && bounds.outHeight / (sample * 2) >= height) {
            sample *= 2
        }
        return BitmapFactory.decodeFile(
            file.path,
            BitmapFactory.Options().apply { inSampleSize = max(1, sample) },
        )
    }
}
