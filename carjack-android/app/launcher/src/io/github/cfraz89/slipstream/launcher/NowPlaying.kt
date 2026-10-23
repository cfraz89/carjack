package io.github.cfraz89.slipstream.launcher

import android.app.Activity
import android.content.ComponentName
import android.graphics.Bitmap
import android.media.MediaMetadata
import android.media.session.MediaController
import android.media.session.MediaSessionManager
import android.media.session.PlaybackState
import android.view.View
import android.widget.ImageView
import android.widget.TextView

/**
 * The home screen's now-playing panel: whichever app is playing (else the most recent session),
 * its title, artist and art, with previous, play/pause and next. Hidden while nothing is, or
 * where [MediaListener] isn't enabled, without which other apps' sessions can't be seen.
 */
class NowPlaying(private val activity: Activity, private val panel: View) {
    private val art = panel.findViewById<ImageView>(R.id.playing_art)
    private val title = panel.findViewById<TextView>(R.id.playing_title)
    private val artist = panel.findViewById<TextView>(R.id.playing_artist)
    private val toggle = panel.findViewById<ImageView>(R.id.playing_toggle)
    private val sessions = activity.getSystemService(MediaSessionManager::class.java)
    private val listener = ComponentName(activity, MediaListener::class.java)
    private var controller: MediaController? = null

    private val sessionsChanged =
        MediaSessionManager.OnActiveSessionsChangedListener { follow(it.orEmpty()) }

    private val changes = object : MediaController.Callback() {
        override fun onMetadataChanged(metadata: MediaMetadata?) = show()
        override fun onPlaybackStateChanged(state: PlaybackState?) {
            // Another session may have taken over playing.
            follow(activeSessions())
        }

        override fun onSessionDestroyed() = follow(activeSessions())
    }

    init {
        art.clipToOutline = true
        panel.findViewById<View>(R.id.playing_previous).setOnClickListener {
            controller?.transportControls?.skipToPrevious()
        }
        panel.findViewById<View>(R.id.playing_next).setOnClickListener {
            controller?.transportControls?.skipToNext()
        }
        toggle.setOnClickListener {
            val controls = controller?.transportControls ?: return@setOnClickListener
            if (isPlaying(controller)) controls.pause() else controls.play()
        }
    }

    fun start() {
        try {
            sessions.addOnActiveSessionsChangedListener(sessionsChanged, listener)
        } catch (e: SecurityException) {
            // The listener isn't enabled (yet): nothing to show.
            panel.visibility = View.GONE
            return
        }
        follow(activeSessions())
    }

    fun stop() {
        sessions.removeOnActiveSessionsChangedListener(sessionsChanged)
        controller?.unregisterCallback(changes)
        controller = null
    }

    private fun activeSessions(): List<MediaController> = try {
        sessions.getActiveSessions(listener)
    } catch (e: SecurityException) {
        emptyList()
    }

    private fun follow(active: List<MediaController>) {
        val next = active.firstOrNull { isPlaying(it) } ?: active.firstOrNull()
        if (next?.sessionToken != controller?.sessionToken) {
            controller?.unregisterCallback(changes)
            controller = next
            next?.registerCallback(changes)
        }
        show()
    }

    private fun show() {
        val playing = controller
        val metadata = playing?.metadata
        val name = metadata?.getString(MediaMetadata.METADATA_KEY_TITLE)
        if (playing == null || name.isNullOrBlank()) {
            panel.visibility = View.GONE
            return
        }
        panel.visibility = View.VISIBLE
        title.text = name
        artist.text = metadata.getString(MediaMetadata.METADATA_KEY_ARTIST)
            ?: metadata.getString(MediaMetadata.METADATA_KEY_ALBUM_ARTIST)
            ?: appName(playing.packageName)
        val cover: Bitmap? = metadata.getBitmap(MediaMetadata.METADATA_KEY_ALBUM_ART)
            ?: metadata.getBitmap(MediaMetadata.METADATA_KEY_ART)
            ?: metadata.getBitmap(MediaMetadata.METADATA_KEY_DISPLAY_ICON)
        if (cover != null) {
            art.setImageBitmap(cover)
        } else {
            art.setImageDrawable(appIcon(playing.packageName))
        }
        toggle.setImageResource(if (isPlaying(playing)) R.drawable.ic_pause else R.drawable.ic_play)
    }

    private fun isPlaying(controller: MediaController?) =
        controller?.playbackState?.state == PlaybackState.STATE_PLAYING

    private fun appName(pkg: String): String = try {
        activity.packageManager.getApplicationLabel(
            activity.packageManager.getApplicationInfo(pkg, 0),
        ).toString()
    } catch (e: Exception) {
        ""
    }

    private fun appIcon(pkg: String) = try {
        activity.packageManager.getApplicationIcon(pkg)
    } catch (e: Exception) {
        null
    }
}
