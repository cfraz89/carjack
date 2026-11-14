package io.github.cfraz89.slipstream

import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.animation.core.FastOutSlowInEasing
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.systemBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.scale
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * The app's front screen: one button that starts a session ([Activation]), or opens
 * [SettingsActivity] until there's what one needs. During one the app opens on the dashboard.
 */
class StartActivity : ComponentActivity() {
    /** Bumped on resume: setup may have changed in the meantime. */
    private var revision by mutableIntStateOf(0)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.setDecorFitsSystemWindows(false)
        setContent { SlipstreamTheme { key(revision) { Screen() } } }
        if (savedInstanceState == null) toDashboard()
    }

    override fun onResume() {
        super.onResume()
        revision++
    }

    /** Gives the task over to the dashboard, the one screen there is during a session. */
    private fun toDashboard() {
        if (!Activation.isOn(this)) return
        startActivity(Activation.dashboard(this))
        finish()
    }

    private fun start() {
        Activation.start(this)
        finish()
    }

    private fun openSettings() {
        startActivity(Intent(this, SettingsActivity::class.java))
    }

    @Composable
    private fun Screen() {
        val ready = Activation.ready(this)
        val colors = MaterialTheme.colorScheme
        Surface(Modifier.fillMaxSize(), color = colors.surface) {
            Box(Modifier.fillMaxSize().systemBarsPadding().padding(16.dp)) {
                Header(Modifier.align(Alignment.TopStart))
                IconButton(::openSettings, Modifier.align(Alignment.TopEnd).padding(top = 20.dp)) {
                    Icon(painterResource(R.drawable.settings_24), "Settings")
                }
                Column(Modifier.align(Alignment.Center), horizontalAlignment = Alignment.CenterHorizontally) {
                    StartButton(ready)
                    Spacer(Modifier.height(28.dp))
                    Text(
                        if (ready) "Start session" else "Set up",
                        color = colors.onSurface,
                        fontSize = 26.sp,
                        fontWeight = FontWeight.SemiBold,
                    )
                    if (!ready) {
                        Spacer(Modifier.height(8.dp))
                        Text(
                            "Slipstream needs a few things first",
                            color = colors.onSurfaceVariant,
                            fontSize = 16.sp,
                            textAlign = TextAlign.Center,
                        )
                    }
                }
            }
        }
    }

    /**
     * Cast-and-play on a disc shaded like the name, pink into blue, with a halo breathing out from
     * it; flat and still until set up, when it opens settings instead.
     */
    @Composable
    private fun StartButton(ready: Boolean) {
        val colors = MaterialTheme.colorScheme
        val breath by rememberInfiniteTransition(label = "halo").animateFloat(
            initialValue = 0f,
            targetValue = 1f,
            animationSpec = infiniteRepeatable(tween(2_400, easing = FastOutSlowInEasing), RepeatMode.Restart),
            label = "breath",
        )
        Box(Modifier.size(280.dp), contentAlignment = Alignment.Center) {
            if (ready) {
                Box(
                    Modifier
                        .size(200.dp)
                        .scale(1f + 0.4f * breath)
                        .alpha(0.5f * (1f - breath))
                        .border(3.dp, colors.tertiary, CircleShape),
                )
            }
            Box(
                Modifier
                    .size(200.dp)
                    .clip(CircleShape)
                    .background(
                        if (ready) {
                            Brush.linearGradient(listOf(colors.tertiary, colors.primary))
                        } else {
                            SolidColor(colors.surfaceContainerHigh)
                        },
                    )
                    .clickable { if (ready) start() else openSettings() },
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    painterResource(if (ready) R.drawable.ic_sl_start else R.drawable.settings_24),
                    if (ready) "Start session" else "Set up",
                    tint = if (ready) Color.White else colors.onSurfaceVariant,
                    modifier = Modifier.size(96.dp),
                )
            }
        }
    }

    @Composable
    private fun Header(modifier: Modifier) {
        Row(modifier.padding(start = 8.dp, top = 24.dp), verticalAlignment = Alignment.CenterVertically) {
            Image(painterResource(R.drawable.ic_sl_app), null, Modifier.size(56.dp))
            Spacer(Modifier.width(16.dp))
            // Yellowtail (Apache 2.0, res/font): a fast script, shaded pink into blue like a trail.
            val colors = MaterialTheme.colorScheme
            Text(
                "Slipstream",
                style = TextStyle(
                    fontFamily = FontFamily(Font(R.font.yellowtail)),
                    fontSize = 40.sp,
                    brush = Brush.horizontalGradient(listOf(colors.tertiary, colors.primary)),
                ),
            )
        }
    }
}
