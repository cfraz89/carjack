package io.github.cfraz89.slipstream.launcher

import android.graphics.drawable.Drawable
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.border
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsFocusedAsState
import androidx.compose.foundation.interaction.collectIsPressedAsState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.ripple
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.composed
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Shadow
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.graphics.drawscope.drawIntoCanvas
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.nativeCanvas
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.res.colorResource
import androidx.compose.ui.res.dimensionResource
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.unit.em
import kotlin.math.max
import kotlin.math.min
import kotlin.math.roundToInt

/** Rajdhani, the instrument-cluster face the launcher is set in (res/font, OFL). */
val Hud = FontFamily(
    Font(R.font.hud_light, FontWeight.Light),
    Font(R.font.hud_medium, FontWeight.Medium),
    Font(R.font.hud_semibold, FontWeight.SemiBold),
)

/** The launcher's palette: Material You's dark tones, from res/values/colors.xml and res/color. */
object Palette {
    val background @Composable get() = colorResource(R.color.background)
    val text @Composable get() = colorResource(R.color.text_primary)
    val textSecondary @Composable get() = colorResource(R.color.text_secondary)
    val accent @Composable get() = colorResource(R.color.accent)
    val onAccent @Composable get() = colorResource(R.color.on_accent)
    val tonal @Composable get() = colorResource(R.color.tonal)
    val onTonal @Composable get() = colorResource(R.color.on_tonal)
    val sheet @Composable get() = colorResource(R.color.sheet)
    val card @Composable get() = colorResource(R.color.slot)
    val divider @Composable get() = colorResource(R.color.divider)
    val ripple @Composable get() = colorResource(R.color.ripple)
}

@Composable
fun LauncherTheme(content: @Composable () -> Unit) {
    MaterialTheme(
        colorScheme = darkColorScheme(
            primary = Palette.accent,
            onPrimary = Palette.onAccent,
            secondaryContainer = Palette.tonal,
            onSecondaryContainer = Palette.onTonal,
            background = Palette.background,
            surface = Palette.background,
            onBackground = Palette.text,
            onSurface = Palette.text,
            onSurfaceVariant = Palette.textSecondary,
        ),
        content = content,
    )
}

/** Text straight on the wallpaper: a soft shadow keeps it legible on a light one. */
@Composable
fun hudText(size: TextUnit, weight: FontWeight = FontWeight.Medium, color: Color = Palette.text) =
    TextStyle(
        fontFamily = Hud,
        fontWeight = weight,
        fontSize = size,
        color = color,
        shadow = Shadow(Color(0x99000000), Offset(0f, 1f), 6f),
    )

/** Date, weather and such: small, spaced capitals. */
@Composable
fun hudCaption(): TextStyle =
    hudText(sp(R.dimen.status_detail_size), color = Palette.textSecondary)
        .copy(letterSpacing = 0.12.em)

/** A text size given in dimens.xml. */
@Composable
fun sp(id: Int): TextUnit = with(LocalDensity.current) { dimensionResource(id).toSp() }

val CardShape @Composable get() = RoundedCornerShape(dimensionResource(R.dimen.corner_radius))

/**
 * Tappable, the launcher's way: a ripple, a shrink that shows at once even when the stream to
 * the unit lags the touch, and an accent ring while the unit's switchgear has it focused - the
 * ripple's own focus tint is too faint to find on a wallpaper at a glance.
 */
@OptIn(ExperimentalFoundationApi::class)
fun Modifier.tappable(
    shape: Shape,
    onLongClick: (() -> Unit)? = null,
    onClick: () -> Unit,
): Modifier = composed {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    val pressed by interaction.collectIsPressedAsState()
    val scale by animateFloatAsState(
        if (pressed) 0.94f else 1f,
        tween(if (pressed) 80 else 140),
        label = "press",
    )
    val ring = if (focused) {
        Modifier.border(dimensionResource(R.dimen.focus_ring_width), Palette.accent, shape)
    } else {
        Modifier
    }
    graphicsLayer {
        scaleX = scale
        scaleY = scale
    }
        .then(ring)
        .clip(shape)
        .combinedClickable(
            interactionSource = interaction,
            indication = ripple(color = Palette.ripple),
            onLongClick = onLongClick,
            onClick = onClick,
        )
}

/**
 * A [Drawable] - an app's icon, the wallpaper - drawn as it is: fitted inside, or with [crop]
 * filling and centred. [tint] recolours it.
 */
@Composable
fun DrawableImage(
    drawable: Drawable?,
    modifier: Modifier = Modifier,
    crop: Boolean = false,
    tint: Color? = null,
) {
    val tintArgb = tint?.toArgb()
    Canvas(modifier) {
        val d = drawable ?: return@Canvas
        val dw = d.intrinsicWidth.takeIf { it > 0 }?.toFloat() ?: size.width
        val dh = d.intrinsicHeight.takeIf { it > 0 }?.toFloat() ?: size.height
        val scale = if (crop) max(size.width / dw, size.height / dh) else min(size.width / dw, size.height / dh)
        val w = dw * scale
        val h = dh * scale
        val left = ((size.width - w) / 2).roundToInt()
        val top = ((size.height - h) / 2).roundToInt()
        drawIntoCanvas { canvas ->
            val bounds = d.copyBounds()
            d.setBounds(left, top, left + w.roundToInt(), top + h.roundToInt())
            if (tintArgb != null) d.setTint(tintArgb)
            canvas.nativeCanvas.save()
            canvas.nativeCanvas.clipRect(0f, 0f, size.width, size.height)
            d.draw(canvas.nativeCanvas)
            canvas.nativeCanvas.restore()
            d.bounds = bounds
        }
    }
}
