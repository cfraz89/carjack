package io.github.cfraz89.slipstream

import android.content.Context
import android.os.Handler
import android.os.HandlerThread
import android.util.Log
import android.view.KeyEvent

/**
 * The head unit's switchgear, in one of two modes. System: up and down move focus through everything
 * on the screen (Shift+Tab, Tab), left is back and right selects. Remote: plain arrows, for apps
 * with remotes of their own (DMD2) that map the keys themselves. A long press on right switches
 * by hand; an app in the remote list switches it on while it's showing. Each change is flashed on
 * the head unit.
 */
object Switchgear {
    private const val TAG = "carjack"
    const val PREF_APPS = "remote_apps"
    /** DMD2, which takes a generic remote. */
    val DEFAULT_APPS = setOf("com.thorkracing.dmd2launcher")
    private const val LONG_PRESS_MS = 600L
    private const val WATCH_MS = 1000L
    /** The head unit lets go of select within a few ms; held a moment, the press shows. */
    private const val SELECT_HOLD_MS = 150L

    /** Keys arrive on the core's threads and the top app is a binder call away: all of it here. */
    private val handler = Handler(HandlerThread("slipstream-switchgear").apply { start() }.looper)
    private var context: Context? = null
    private var remote = false
    private var lastApp: String? = null
    private var longPressed = false

    fun apps(context: Context): Set<String> =
        Variant.prefs(context).getStringSet(PREF_APPS, DEFAULT_APPS) ?: DEFAULT_APPS

    fun setApps(context: Context, apps: Set<String>) {
        Variant.prefs(context).edit().putStringSet(PREF_APPS, apps).apply()
    }

    /** From the head unit's display opening: follows the app on it, starting in system mode. */
    fun watch(context: Context) {
        handler.post {
            this.context = context.applicationContext
            remote = false
            lastApp = null
            handler.removeCallbacks(poll)
            handler.post(poll)
        }
    }

    fun stop() {
        handler.post {
            handler.removeCallbacks(poll)
            context = null
            remote = false
        }
    }

    /** Only on a change of app, so a mode switched by hand holds while the app does. */
    private val poll = object : Runnable {
        override fun run() {
            val ctx = context ?: return
            val app = ShizukuDisplay.topPackage()
            if (app != null && app != lastApp) {
                lastApp = app
                setRemote(app in apps(ctx))
            }
            handler.postDelayed(this, WATCH_MS)
        }
    }

    private fun setRemote(on: Boolean) {
        if (on == remote) return
        remote = on
        Log.i(TAG, "Switchgear in ${if (on) "remote" else "system"} mode")
        ScreenStream.current?.flash(if (on) "REMOTE MODE" else "SYSTEM MODE")
    }

    fun key(context: Context, code: Int, down: Boolean) {
        handler.post { handle(context, code, down) }
    }

    private fun handle(context: Context, code: Int, down: Boolean) {
        when (code) {
            KeyEvent.KEYCODE_DPAD_CENTER -> select(context, down)
            KeyEvent.KEYCODE_BACK -> if (remote) {
                Variant.key(context, KeyEvent.KEYCODE_DPAD_LEFT, down)
            } else {
                if (down) ScreenStream.current?.cue(true)
                Variant.key(context, code, down)
            }
            // Focus then visits everything on the screen in reading order, wrapping round, where
            // the arrows only reach what lies above or below.
            KeyEvent.KEYCODE_DPAD_DOWN -> if (remote) {
                Variant.key(context, code, down)
            } else {
                Variant.inject(KeyEvent.KEYCODE_TAB, down, 0)
            }
            KeyEvent.KEYCODE_DPAD_UP -> if (remote) {
                Variant.key(context, code, down)
            } else {
                Variant.inject(
                    KeyEvent.KEYCODE_TAB, down, KeyEvent.META_SHIFT_ON or KeyEvent.META_SHIFT_LEFT_ON,
                )
            }
            else -> Variant.key(context, code, down)
        }
    }

    /** Sent at release, once it's known not to be a long press. */
    private fun select(context: Context, down: Boolean) {
        if (down) {
            longPressed = false
            handler.postDelayed(longPress, LONG_PRESS_MS)
            return
        }
        handler.removeCallbacks(longPress)
        if (longPressed) return
        if (remote) {
            Variant.key(context, KeyEvent.KEYCODE_DPAD_RIGHT, true)
            Variant.key(context, KeyEvent.KEYCODE_DPAD_RIGHT, false)
        } else {
            ScreenStream.current?.cue(false)
            Variant.key(context, KeyEvent.KEYCODE_DPAD_CENTER, true)
            handler.postDelayed(
                { Variant.key(context, KeyEvent.KEYCODE_DPAD_CENTER, false) }, SELECT_HOLD_MS,
            )
        }
    }

    private val longPress = Runnable {
        longPressed = true
        setRemote(!remote)
    }
}
