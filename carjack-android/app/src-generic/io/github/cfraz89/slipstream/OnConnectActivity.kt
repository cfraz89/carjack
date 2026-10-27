package io.github.cfraz89.slipstream

import android.content.Intent
import android.graphics.BitmapFactory
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import io.github.cfraz89.slipstream.launcher.DrawableImage

/**
 * What the head unit opens on connect, picked like a wallpaper: a large thumbnail per choice, the last
 * picture that choice put on the head unit, or its icon until it has been shown there.
 */
class OnConnectActivity : ComponentActivity() {
    private companion object {
        /** The head unit's picture, which the thumbnails keep the shape of. */
        const val ASPECT = 800f / 1056f
    }

    private var revision by mutableIntStateOf(0)

    private val pickApp = registerForActivityResult(ActivityResultContracts.StartActivityForResult()) {
        val pkg = it.data?.getStringExtra(AppPickerActivity.EXTRA_PACKAGE)
        if (it.resultCode == RESULT_OK && pkg != null) {
            Choices.choose(this, pkg)
            revision++
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.setDecorFitsSystemWindows(false)
        setContent { SlipstreamTheme { key(revision) { Screen() } } }
    }

    override fun onResume() {
        super.onResume()
        revision++
    }

    @Composable
    private fun Screen() {
        Page {
            TopBar("On connect") { finish() }
            Text(
                "What the head unit shows when a session starts",
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                fontSize = 14.sp,
                modifier = Modifier.padding(start = 16.dp, end = 16.dp, bottom = 8.dp),
            )
            val current = Choices.current(this@OnConnectActivity)
            val choices = listOf(Choices.LAUNCHER, Previews.HOME, current.takeIf(Choices::isApp))
            choices.chunked(2).forEach { line ->
                Row(Modifier.fillMaxWidth()) {
                    line.forEach { choice ->
                        Box(Modifier.weight(1f).padding(6.dp)) { Card(choice, current) }
                    }
                    // Keeps a lone last card half wide.
                    if (line.size == 1) Spacer(Modifier.weight(1f))
                }
            }
        }
    }

    /** One choice; null is "an app", not yet chosen. */
    @Composable
    private fun Card(choice: String?, current: String) {
        val context = this
        val colors = MaterialTheme.colorScheme
        val selected = choice == current
        val name = choice?.let { Choices.name(context, it) } ?: "An app"
        val detail = when {
            choice == null -> "Choose one"
            Choices.isApp(choice) -> "Tap to choose another"
            else -> Choices.detail(context, choice)
        }
        Column(
            Modifier
                .fillMaxWidth()
                .clip(RoundedCornerShape(24.dp))
                .background(if (selected) colors.primaryContainer else colors.surfaceContainer)
                .clickable {
                    if (choice == null || Choices.isApp(choice)) {
                        pickApp.launch(Intent(context, AppPickerActivity::class.java))
                    } else {
                        Choices.choose(context, choice)
                        revision++
                    }
                }
                .padding(start = 6.dp, end = 6.dp, top = 6.dp, bottom = 10.dp),
        ) {
            val thumb = RoundedCornerShape(18.dp)
            Box(
                Modifier
                    .fillMaxWidth()
                    .aspectRatio(ASPECT)
                    .clip(thumb)
                    .background(colors.surfaceContainerHighest)
                    .then(if (selected) Modifier.border(3.dp, colors.primary, thumb) else Modifier),
                contentAlignment = Alignment.Center,
            ) {
                val preview = choice?.let { Previews.file(context, it) }
                    ?.takeIf { it.exists() }
                    ?.let { BitmapFactory.decodeFile(it.path) }
                if (preview != null) {
                    Image(
                        preview.asImageBitmap(), null,
                        Modifier.fillMaxSize(), contentScale = ContentScale.Crop,
                    )
                } else {
                    ChoiceIcon(choice, Modifier.size(72.dp))
                }
                if (selected) {
                    Icon(
                        painterResource(R.drawable.ic_sl_check), null,
                        tint = colors.primary,
                        modifier = Modifier
                            .align(Alignment.TopEnd)
                            .padding(10.dp)
                            .size(28.dp)
                            .background(colors.primaryContainer, RoundedCornerShape(14.dp)),
                    )
                }
            }
            Row(
                Modifier.padding(start = 6.dp, end = 6.dp, top = 10.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                ChoiceIcon(choice, Modifier.size(24.dp))
                Spacer(Modifier.width(8.dp))
                Text(
                    name,
                    color = if (selected) colors.onPrimaryContainer else colors.onSurface,
                    fontSize = 15.sp,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            if (detail != null) {
                Text(
                    detail,
                    color = colors.onSurfaceVariant,
                    fontSize = 13.sp,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.padding(start = 38.dp, end = 6.dp, top = 2.dp),
                )
            }
        }
    }

    /** The choice's own icon; a plain glyph, in the accent, where it has none. */
    @Composable
    private fun ChoiceIcon(choice: String?, modifier: Modifier) {
        val primary = MaterialTheme.colorScheme.primary
        if (choice == null) {
            Icon(painterResource(R.drawable.ic_sl_apps), null, modifier, tint = primary)
            return
        }
        val glyph = choice == Previews.HOME && Variant.homeApp(this) == null
        DrawableImage(Choices.icon(this, choice), modifier, tint = if (glyph) primary else null)
    }
}
