package io.github.cfraz89.slipstream.launcher

import android.content.ComponentName
import android.content.Context
import android.graphics.Bitmap
import android.graphics.drawable.BitmapDrawable
import android.graphics.drawable.Drawable
import android.media.MediaMetadata
import android.media.session.MediaController
import android.media.session.MediaSessionManager
import android.media.session.PlaybackState
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.res.dimensionResource
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow

/**
 * The home screen's now-playing panel: whichever app is playing (else the most recent session),
 * its title, artist and art, with previous, play/pause and next. Hidden while nothing is, or
 * where [MediaListener] isn't enabled, without which other apps' sessions can't be seen.
 */
class NowPlaying(private val context: Context) {
    /** What's showing; null hides the panel. */
    data class Track(val title: String, val artist: String, val art: Drawable?, val playing: Boolean)

    var track by mutableStateOf<Track?>(null)
        private set

    private val sessions = context.getSystemService(MediaSessionManager::class.java)
    private val listener = ComponentName(context, MediaListener::class.java)
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

    fun start() {
        try {
            sessions.addOnActiveSessionsChangedListener(sessionsChanged, listener)
        } catch (e: SecurityException) {
            // The listener isn't enabled (yet): nothing to show.
            track = null
            return
        }
        follow(activeSessions())
    }

    fun stop() {
        sessions.removeOnActiveSessionsChangedListener(sessionsChanged)
        controller?.unregisterCallback(changes)
        controller = null
    }

    fun previous() {
        controller?.transportControls?.skipToPrevious()
    }

    fun next() {
        controller?.transportControls?.skipToNext()
    }

    fun toggle() {
        val controls = controller?.transportControls ?: return
        if (isPlaying(controller)) controls.pause() else controls.play()
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
            track = null
            return
        }
        val cover: Bitmap? = metadata.getBitmap(MediaMetadata.METADATA_KEY_ALBUM_ART)
            ?: metadata.getBitmap(MediaMetadata.METADATA_KEY_ART)
            ?: metadata.getBitmap(MediaMetadata.METADATA_KEY_DISPLAY_ICON)
        track = Track(
            title = name,
            artist = metadata.getString(MediaMetadata.METADATA_KEY_ARTIST)
                ?: metadata.getString(MediaMetadata.METADATA_KEY_ALBUM_ARTIST)
                ?: appName(playing.packageName),
            art = cover?.let { BitmapDrawable(context.resources, it) }
                ?: appIcon(playing.packageName),
            playing = isPlaying(playing),
        )
    }

    private fun isPlaying(controller: MediaController?) =
        controller?.playbackState?.state == PlaybackState.STATE_PLAYING

    private fun appName(pkg: String): String = try {
        context.packageManager.getApplicationLabel(
            context.packageManager.getApplicationInfo(pkg, 0),
        ).toString()
    } catch (e: Exception) {
        ""
    }

    private fun appIcon(pkg: String) = try {
        context.packageManager.getApplicationIcon(pkg)
    } catch (e: Exception) {
        null
    }
}

@Composable
fun NowPlayingPanel(playing: NowPlaying) {
    val track = playing.track ?: return
    Row(
        Modifier
            .fillMaxWidth()
            .padding(
                start = dimensionResource(R.dimen.status_padding),
                end = dimensionResource(R.dimen.playing_padding_end),
                top = dimensionResource(R.dimen.playing_padding_top),
                bottom = dimensionResource(R.dimen.playing_padding_bottom),
            ),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        DrawableImage(
            track.art,
            Modifier
                .size(dimensionResource(R.dimen.playing_art_size))
                .clip(CardShape)
                .background(Palette.card),
            crop = true,
        )
        Spacer(Modifier.width(dimensionResource(R.dimen.playing_text_gap)))
        Column(Modifier.weight(1f)) {
            Text(
                track.title,
                style = hudText(sp(R.dimen.playing_title_size), FontWeight.SemiBold),
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                track.artist.uppercase(),
                style = hudCaption(),
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        PlayingButton(R.drawable.ic_skip_previous, R.string.playing_previous) { playing.previous() }
        PlayingButton(
            if (track.playing) R.drawable.ic_pause else R.drawable.ic_play,
            R.string.playing_toggle,
        ) { playing.toggle() }
        PlayingButton(R.drawable.ic_skip_next, R.string.playing_next) { playing.next() }
    }
}

@Composable
private fun PlayingButton(icon: Int, label: Int, onClick: () -> Unit) {
    Icon(
        painterResource(icon),
        contentDescription = stringResource(label),
        tint = Palette.text,
        modifier = Modifier
            .size(dimensionResource(R.dimen.playing_button_size))
            .tappable(CircleShape, onClick = onClick)
            .padding(dimensionResource(R.dimen.playing_button_padding)),
    )
}
