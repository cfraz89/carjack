// Adapted from MoonClicker (MIT, Martin Kuo): engine/.../service/VirtualDevices.kt, and its
// docs/research/vd-display-group-android17.md.
package io.github.cfraz89.slipstream

import android.content.AttributionSource
import android.content.Context
import android.content.ContextWrapper
import android.hardware.display.VirtualDisplay
import android.hardware.display.VirtualDisplayConfig
import android.os.IBinder
import android.os.Process
import android.util.Log
import android.view.Surface

/**
 * A display made by a VirtualDevice, which gets a display group and power group of its own -
 * awake while the phone's screen is off, its touches still taken - and with the device always
 * unlocked, no lock screen. Android 17 puts any other virtual display in the phone's group
 * (`separate_timeouts`), asleep and locked with it. A VirtualDevice needs a companion association;
 * the shell has none, so `cmd companiondevice` makes it one for a made-up MAC. Runs as the shell,
 * which holds CREATE_VIRTUAL_DEVICE and ADD_ALWAYS_UNLOCKED_DISPLAY. Hidden APIs by reflection.
 */
class VirtualDevices(private val context: Context) {
    /** The display, and what closes its device, and the display with it. */
    class Created(val display: VirtualDisplay, val close: () -> Unit)

    private companion object {
        const val TAG = "carjack"
        const val SHELL = "com.android.shell"
        const val PROFILE = "android.app.role.COMPANION_DEVICE_APP_STREAMING"
        /** Locally administered; "73:6c" is "sl". */
        const val MAC = "02:73:6c:00:00:01"
        /** `VirtualDeviceParams.LOCK_STATE_ALWAYS_UNLOCKED`. */
        const val LOCK_STATE_ALWAYS_UNLOCKED = 1
        /** `UserHandle.PER_USER_RANGE`. */
        const val PER_USER_RANGE = 100000
        const val POLL_ATTEMPTS = 20
        const val POLL_MS = 50L
    }

    private val userId = Process.myUid() / PER_USER_RANGE

    /**
     * The display, or null where this Android can't make one this way; the caller falls back to
     * an ordinary virtual display.
     */
    fun create(name: String, width: Int, height: Int, density: Int, surface: Surface, flags: Int): Created? = try {
        val association = association() ?: associate()
        if (association == null) {
            Log.w(TAG, "No companion association for a VirtualDevice")
            null
        } else {
            createOn(association, name, width, height, density, surface, flags)
        }
    } catch (e: ReflectiveOperationException) {
        Log.w(TAG, "No VirtualDevice display", e)
        null
    } catch (e: java.io.IOException) {
        Log.w(TAG, "No VirtualDevice display", e)
        null
    } catch (e: RuntimeException) {
        Log.w(TAG, "No VirtualDevice display", e)
        null
    }

    private fun createOn(
        association: Int, name: String, width: Int, height: Int, density: Int, surface: Surface, flags: Int,
    ): Created? {
        val managerClass = Class.forName("android.companion.virtual.VirtualDeviceManager")
        val serviceClass = Class.forName("android.companion.virtual.IVirtualDeviceManager")
        val service = Class.forName("android.companion.virtual.IVirtualDeviceManager\$Stub")
            .getMethod("asInterface", IBinder::class.java).invoke(null, binder("virtualdevice"))
        val manager = managerClass.getDeclaredConstructor(serviceClass, Context::class.java)
            .apply { isAccessible = true }.newInstance(service, ShellContext(context))

        val builderClass = Class.forName("android.companion.virtual.VirtualDeviceParams\$Builder")
        val builder = builderClass.getConstructor().newInstance()
        builderClass.getMethod("setLockState", Int::class.javaPrimitiveType).invoke(builder, LOCK_STATE_ALWAYS_UNLOCKED)
        val params = builderClass.getMethod("build").invoke(builder)
        val paramsClass = Class.forName("android.companion.virtual.VirtualDeviceParams")
        val device = managerClass.getMethod("createVirtualDevice", Int::class.javaPrimitiveType, paramsClass)
            .invoke(manager, association, params)!!
        val close = { device.javaClass.getMethod("close").invoke(device); Unit }

        val config = VirtualDisplayConfig.Builder(name, width, height, density)
            .setSurface(surface)
            .setFlags(flags)
            .build()
        val display = try {
            device.javaClass.getMethod(
                "createVirtualDisplay",
                VirtualDisplayConfig::class.java,
                java.util.concurrent.Executor::class.java,
                VirtualDisplay.Callback::class.java,
            ).invoke(device, config, null, null) as VirtualDisplay?
        } catch (e: ReflectiveOperationException) {
            close()
            throw e
        }
        if (display == null) {
            close()
            return null
        }
        return Created(display, close)
    }

    /** The association made for [MAC] before, if any. */
    private fun association(): Int? {
        val cdm = Class.forName("android.companion.ICompanionDeviceManager\$Stub")
            .getMethod("asInterface", IBinder::class.java).invoke(null, binder(Context.COMPANION_DEVICE_SERVICE))!!
        val list = cdm.javaClass.getMethod("getAssociations", String::class.java, Int::class.javaPrimitiveType)
            .invoke(cdm, SHELL, userId) as List<*>
        for (info in list) {
            info ?: continue
            val mac = info.javaClass.getMethod("getDeviceMacAddress").invoke(info)?.toString()
            if (mac.equals(MAC, ignoreCase = true)) return info.javaClass.getMethod("getId").invoke(info) as Int
        }
        return null
    }

    /** The command returns before the association is stored, so it's waited for. */
    private fun associate(): Int? {
        val process = ProcessBuilder("cmd", "companiondevice", "associate", userId.toString(), SHELL, MAC, PROFILE, "false")
            .redirectErrorStream(true).start()
        val out = process.inputStream.bufferedReader().use { it.readText() }.trim()
        process.waitFor()
        Log.i(TAG, "Companion association for the head unit's display: $out")
        repeat(POLL_ATTEMPTS) {
            association()?.let { return it }
            Thread.sleep(POLL_MS)
        }
        return null
    }

    private fun binder(name: String) = Class.forName("android.os.ServiceManager")
        .getMethod("getService", String::class.java).invoke(null, name) as IBinder

    /** The shell's package, as the VirtualDevice service reads it, whichever way it asks. */
    private class ShellContext(base: Context) : ContextWrapper(base) {
        override fun getPackageName() = SHELL
        override fun getOpPackageName() = SHELL
        override fun getApplicationContext(): Context = this
        override fun getAttributionSource(): AttributionSource =
            AttributionSource.Builder(Process.myUid()).setPackageName(SHELL).build()
    }
}
