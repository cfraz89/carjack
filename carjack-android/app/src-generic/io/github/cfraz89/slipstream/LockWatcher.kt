package io.github.cfraz89.slipstream

import android.app.KeyguardManager
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import android.os.Handler
import android.os.HandlerThread
import android.os.SystemClock
import android.util.Log

/**
 * The phone's lock screen, during a session. While it's up, Android shows only windows that may
 * show over it (the launcher) on the head unit's display, which shares the phone's display group, and
 * every other app draws black. A lock screen that needs no PIN - a trusted device's - is taken
 * away; one that does gets [LockedActivity] on the head unit and a notification on the phone, both gone
 * at unlock.
 */
object LockWatcher {
    private const val TAG = "carjack"
    private const val POLL_MS = 1000L
    /** Between tries at a lock screen that should go but hasn't. */
    private const val DISMISS_RETRY_MS = 5000L
    private const val NOTICE_ID = 3
    private const val CHANNEL = "locked"
    private const val LOCKED = "${Variant.APP_ID}/${Variant.APP_ID}.LockedActivity"

    private val handler = Handler(HandlerThread("slipstream-lock").apply { start() }.looper)
    private var context: Context? = null
    private var lastDismiss = 0L
    private var shown = false

    /** From the head unit's display opening. */
    fun watch(context: Context) {
        handler.post {
            this.context = context.applicationContext
            lastDismiss = 0
            shown = false
            handler.removeCallbacks(poll)
            handler.post(poll)
        }
    }

    fun stop() {
        handler.post {
            handler.removeCallbacks(poll)
            context?.let(::hide)
            context = null
        }
    }

    private val poll = object : Runnable {
        override fun run() {
            val ctx = context ?: return
            val keyguard = ctx.getSystemService(KeyguardManager::class.java)
            val now = SystemClock.uptimeMillis()
            when {
                !keyguard.isKeyguardLocked -> hide(ctx)
                // Trusted, so the lock screen is only a swipe: it goes.
                !keyguard.isDeviceLocked -> if (now - lastDismiss >= DISMISS_RETRY_MS) {
                    lastDismiss = now
                    Log.i(TAG, "Lock screen up but the phone's trusted; dismissing it")
                    ShizukuDisplay.dismissKeyguard()
                }
                !shown -> {
                    shown = true
                    Log.i(TAG, "Phone locked; the head unit shows so until it's unlocked")
                    ShizukuDisplay.launch(LOCKED)
                    notify(ctx)
                }
            }
            handler.postDelayed(this, POLL_MS)
        }
    }

    private fun hide(context: Context) {
        if (!shown) return
        shown = false
        context.getSystemService(NotificationManager::class.java).cancel(NOTICE_ID)
        LockedActivity.close()
    }

    /** Shown on the lock screen, so it's of default importance; silent all the same. */
    private fun notify(context: Context) {
        val manager = context.getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(
            NotificationChannel(CHANNEL, "Phone locked", NotificationManager.IMPORTANCE_DEFAULT).apply {
                setSound(null, null)
                enableVibration(false)
            },
        )
        manager.notify(
            NOTICE_ID,
            Notification.Builder(context, CHANNEL)
                .setSmallIcon(Variant.NOTIFICATION_ICON)
                .setContentTitle("Phone locked")
                .setContentText("Unlock it to use apps on the head unit")
                .setContentIntent(Variant.openApp(context))
                .setAutoCancel(true)
                .setVisibility(Notification.VISIBILITY_PUBLIC)
                .build(),
        )
    }
}
