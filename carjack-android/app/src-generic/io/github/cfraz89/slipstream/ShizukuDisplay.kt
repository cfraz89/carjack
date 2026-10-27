package io.github.cfraz89.slipstream

import android.content.ComponentName
import android.content.ServiceConnection
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.BitmapRegionDecoder
import android.graphics.Rect
import android.os.IBinder
import android.os.ParcelFileDescriptor
import android.os.RemoteException
import android.util.Log
import android.view.Surface
import rikka.shizuku.Shizuku
import java.io.IOException
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

/**
 * A display of the unit's own size with apps running on it, made by [DisplayService] through
 * Shizuku. Used in place of the screen mirror where Shizuku is running and has let Slipstream in.
 */
object ShizukuDisplay {
    private const val TAG = "carjack"
    private const val BIND_TIMEOUT_S = 10L
    private val MAC = Regex("([0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}")

    private val ARGS = Shizuku.UserServiceArgs(
        ComponentName(Variant.APP_ID, DisplayService::class.java.name),
    )
        .daemon(false)
        .processNameSuffix("display")
        .debuggable(false)
        .version(34)

    @Volatile private var service: IDisplayService? = null
    private var bound = CountDownLatch(1)
    /** Set while a display is up; touches go to it. */
    @Volatile var active: IDisplayService? = null

    private val CONNECTION = object : ServiceConnection {
        override fun onServiceConnected(name: ComponentName, binder: IBinder) {
            service = IDisplayService.Stub.asInterface(binder)
            bound.countDown()
        }

        override fun onServiceDisconnected(name: ComponentName) {
            service = null
            active = null
            bound = CountDownLatch(1)
        }
    }

    /** Shizuku is running and has granted Slipstream access. */
    fun available(): Boolean = try {
        Shizuku.pingBinder() && Shizuku.checkSelfPermission() == PackageManager.PERMISSION_GRANTED
    } catch (e: RuntimeException) {
        false
    }

    /** [call] on the service where Shizuku is up; [fallback] where it isn't, or the call fails. */
    private inline fun <T> withService(fallback: T, call: (IDisplayService) -> T): T {
        val s = (if (available()) bind() else null) ?: return fallback
        return try {
            call(s)
        } catch (e: RemoteException) {
            fallback
        }
    }

    /**
     * A display drawing into [target] with [app] (a component or package) on it; returns what
     * closes it, or null where Shizuku can't make one. [keepPanel]: the phone's panel goes dark
     * rather than the phone sleeping, meanwhile.
     */
    fun open(
        target: Surface, width: Int, height: Int, density: Int, app: String, keepPanel: Boolean,
    ): Runnable? {
        val s = bind() ?: return null
        return try {
            if (s.create(target, width, height, density) < 0) return null
            s.launch(app)
            Log.i(
                TAG,
                "Display up; the phone's panel " + if (keepPanel) {
                    "goes dark instead of the phone sleeping"
                } else {
                    "sleeps with the phone, as usual"
                },
            )
            if (keepPanel) s.keepPanel(true)
            active = s
            Runnable {
                active = null
                try {
                    s.release()
                } catch (ignored: RemoteException) {
                }
            }
        } catch (e: RemoteException) {
            Log.w(TAG, "Shizuku display service failed", e)
            null
        }
    }

    /** The app showing on the unit's display, while there is one. */
    fun topPackage(): String? = try {
        active?.topPackage()
    } catch (e: RemoteException) {
        null
    }

    /** Bluetooth off and on through Shizuku; false where Shizuku can't. */
    fun cycleBluetooth(): Boolean = withService(false) {
        it.cycleBluetooth()
        true
    }

    /** A network interface's MAC through Shizuku, or null. */
    fun interfaceMac(iface: String): String? =
        withService(null) { s -> s.interfaceMac(iface)?.takeIf { MAC.matches(it) } }

    /** [IDisplayService.joinPrimary]; false where Shizuku can't. Blocks up to ~20 s. */
    fun joinPrimary(ssid: String, passphrase: String?): Boolean =
        withService(false) { it.joinPrimary(ssid, passphrase) }

    fun leavePrimary(ssid: String?) {
        withService(Unit) { it.leavePrimary(ssid) }
    }

    /**
     * The phone's wallpaper as the system would frame it for a [width]x[height] display, about
     * that size; null where Shizuku can't read it (a live wallpaper, say). Blocks.
     */
    fun framedWallpaper(width: Int, height: Int): Bitmap? {
        val s = (if (available()) bind() else null) ?: return null
        return try {
            s.wallpaperOriginal()?.use { original ->
                val decoder = BitmapRegionDecoder.newInstance(original)
                val c = s.wallpaperCrop(width, height)
                val crop = if (c != null && c.size == 4) {
                    Rect(c[0], c[1], c[2], c[3])
                } else {
                    Rect(0, 0, decoder.width, decoder.height)
                }
                crop.intersect(0, 0, decoder.width, decoder.height)
                val options = BitmapFactory.Options().apply { inSampleSize = 1 }
                while (crop.width() / (options.inSampleSize * 2) >= width &&
                    crop.height() / (options.inSampleSize * 2) >= height
                ) {
                    options.inSampleSize *= 2
                }
                Log.i(
                    TAG,
                    "Wallpaper for ${width}x$height: $crop of ${decoder.width}x${decoder.height}",
                )
                val bitmap = decoder.decodeRegion(crop, options)
                decoder.recycle()
                bitmap
            }
        } catch (e: RemoteException) {
            Log.w(TAG, "No framed wallpaper", e)
            null
        } catch (e: IOException) {
            Log.w(TAG, "No framed wallpaper", e)
            null
        } catch (e: RuntimeException) {
            Log.w(TAG, "No framed wallpaper", e)
            null
        }
    }

    /**
     * [IDisplayService.secureSetting]; [fallback] where Shizuku can't say, which callers tell from
     * a real answer by identity.
     */
    fun secureSetting(key: String, fallback: String?): String? =
        withService(fallback) { it.secureSetting(key) }

    /** False where Shizuku couldn't set it. */
    fun setSecureSetting(key: String, value: String?): Boolean = withService(false) {
        it.setSecureSetting(key, value)
        true
    }

    /** [IDisplayService.trusted]; null where Shizuku can't say. */
    fun trusted(): Boolean? = withService(null) { it.trusted() }

    /** Part [part] of the phone's system log; null if none, or without Shizuku. */
    fun systemLog(part: Int): ParcelFileDescriptor? = withService(null) { it.systemLog(part) }

    /** The phone's real Bluetooth address through Shizuku, or null. */
    fun bluetoothAddress(): String? =
        withService(null) { s -> s.bluetoothAddress()?.takeIf { MAC.matches(it) } }

    /**
     * What the user would otherwise be asked for: the app's runtime permissions, and Bluetooth
     * on. Blocks while Shizuku starts the service, so not on the main thread.
     */
    fun setUp(
        pkg: String, permissions: Array<String>, enableBluetooth: Boolean,
        notificationListener: String?,
    ) {
        val s = (if (available()) bind() else null) ?: return
        try {
            s.grant(pkg, permissions)
            if (enableBluetooth) s.enableBluetooth()
            if (notificationListener != null) s.allowNotificationListener(notificationListener)
        } catch (e: RemoteException) {
            Log.w(TAG, "Shizuku couldn't set Slipstream up", e)
        }
    }

    private fun bind(): IDisplayService? {
        if (service == null) {
            try {
                Shizuku.bindUserService(ARGS, CONNECTION)
                bound.await(BIND_TIMEOUT_S, TimeUnit.SECONDS)
            } catch (e: RuntimeException) {
                Log.w(TAG, "Could not start the Shizuku display service", e)
            } catch (e: InterruptedException) {
                Thread.currentThread().interrupt()
            }
        }
        return service
    }

    fun unbind() {
        active = null
        try {
            Shizuku.unbindUserService(ARGS, CONNECTION, true)
        } catch (ignored: RuntimeException) {
        }
        service = null
        bound = CountDownLatch(1)
    }
}
