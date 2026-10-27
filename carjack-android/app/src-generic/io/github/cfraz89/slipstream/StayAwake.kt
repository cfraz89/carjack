package io.github.cfraz89.slipstream

import android.content.Context
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.PowerManager
import android.util.Log

/**
 * Keeps the phone's CPU from suspending for as long as the head unit has a display, leaving its screen
 * to do as it would. Runs in the Shizuku process, as the shell user.
 */
class StayAwake(private val shell: Context) {
    private companion object {
        const val TAG = "carjack"
    }

    private var wakeLock: PowerManager.WakeLock? = null

    fun start() {
        try {
            wakeLock = powerManager().newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "carjack:display")
                .also { it.acquire() }
        } catch (e: ReflectiveOperationException) {
            Log.w(TAG, "Could not keep the phone awake", e)
        } catch (e: RuntimeException) {
            Log.w(TAG, "Could not keep the phone awake", e)
        }
    }

    fun stop() {
        wakeLock?.release()
        wakeLock = null
    }

    /** One that names the shell, which the wake lock is attributed to. */
    private fun powerManager(): PowerManager {
        val binder = Class.forName("android.os.ServiceManager")
            .getMethod("getService", String::class.java)
            .invoke(null, Context.POWER_SERVICE)
        val service = Class.forName("android.os.IPowerManager\$Stub")
            .getMethod("asInterface", IBinder::class.java)
            .invoke(null, binder)
        val looper = Looper.getMainLooper() ?: Looper.myLooper()
        return PowerManager::class.java.getDeclaredConstructor(
            Context::class.java,
            Class.forName("android.os.IPowerManager"),
            Class.forName("android.os.IThermalService"),
            Handler::class.java,
        ).newInstance(shell, service, null, looper?.let { Handler(it) })
    }
}
