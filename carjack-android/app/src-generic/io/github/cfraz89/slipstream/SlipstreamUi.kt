package io.github.cfraz89.slipstream

import android.graphics.drawable.Drawable
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.systemBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.wrapContentSize
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.lightColorScheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.colorResource
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import io.github.cfraz89.slipstream.launcher.DrawableImage

/** Material 3 in Slipstream's palette (res/values/colors.xml, dark in values-night). */
@Composable
fun SlipstreamTheme(content: @Composable () -> Unit) {
    val primary = colorResource(R.color.sl_primary)
    val onPrimary = colorResource(R.color.sl_on_primary)
    val primaryContainer = colorResource(R.color.sl_primary_container)
    val onPrimaryContainer = colorResource(R.color.sl_on_primary_container)
    val tertiary = colorResource(R.color.sl_tertiary)
    val onTertiary = colorResource(R.color.sl_on_tertiary)
    val tertiaryContainer = colorResource(R.color.sl_tertiary_container)
    val onTertiaryContainer = colorResource(R.color.sl_on_tertiary_container)
    val surface = colorResource(R.color.sl_surface)
    val onSurface = colorResource(R.color.sl_on_surface)
    val onSurfaceVariant = colorResource(R.color.sl_on_surface_variant)
    val outline = colorResource(R.color.sl_outline)
    val outlineVariant = colorResource(R.color.sl_outline_variant)
    val container = colorResource(R.color.sl_surface_container)
    val containerHigh = colorResource(R.color.sl_surface_container_high)
    val containerHighest = colorResource(R.color.sl_surface_container_highest)
    val base = if (isSystemInDarkTheme()) darkColorScheme() else lightColorScheme()
    MaterialTheme(
        colorScheme = base.copy(
            primary = primary,
            onPrimary = onPrimary,
            primaryContainer = primaryContainer,
            onPrimaryContainer = onPrimaryContainer,
            tertiary = tertiary,
            onTertiary = onTertiary,
            tertiaryContainer = tertiaryContainer,
            onTertiaryContainer = onTertiaryContainer,
            background = surface,
            surface = surface,
            onBackground = onSurface,
            onSurface = onSurface,
            onSurfaceVariant = onSurfaceVariant,
            outline = outline,
            outlineVariant = outlineVariant,
            surfaceContainer = container,
            surfaceContainerHigh = containerHigh,
            surfaceContainerHighest = containerHighest,
        ),
        content = content,
    )
}

/** A scrolling page drawn edge to edge, kept clear of the system bars. */
@Composable
fun Page(content: @Composable ColumnScope.() -> Unit) {
    // A Surface, not just a background: it also sets the text and icon colour beneath it.
    Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.surface) {
        Column(
            Modifier
                .fillMaxSize()
                .systemBarsPadding()
                .verticalScroll(rememberScrollState())
                .padding(start = 16.dp, end = 16.dp, top = 8.dp, bottom = 24.dp),
            content = content,
        )
    }
}

/** A setup step's explanation: what it's for, then what to do, step by step. */
class Help(val title: String, val why: String, val steps: List<String>)

/** [help] as a screen of its own, over whatever opened it. */
@Composable
fun HelpScreen(help: Help, onClose: () -> Unit) {
    Dialog(onDismissRequest = onClose, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        Page {
            TopBar(help.title, onClose)
            Text(
                help.why,
                fontSize = 16.sp,
                modifier = Modifier.padding(start = 16.dp, end = 16.dp, bottom = 16.dp),
            )
            SectionCard(
                MaterialTheme.colorScheme.surfaceContainer,
                padding = PaddingValues(start = 20.dp, end = 20.dp, top = 16.dp, bottom = 16.dp),
            ) {
                help.steps.forEachIndexed { index, step ->
                    Row(Modifier.padding(top = if (index == 0) 0.dp else 14.dp)) {
                        Text(
                            "${index + 1}",
                            color = MaterialTheme.colorScheme.onTertiary,
                            fontSize = 13.sp,
                            modifier = Modifier
                                .size(22.dp)
                                .background(MaterialTheme.colorScheme.tertiary, CircleShape)
                                .wrapContentSize(Alignment.Center),
                        )
                        Spacer(Modifier.width(12.dp))
                        Text(step, fontSize = 15.sp)
                    }
                }
            }
        }
    }
}

/** M3's small top app bar: back, then the title. */
@Composable
fun TopBar(title: String, onBack: () -> Unit) {
    Row(Modifier.heightIn(min = 64.dp), verticalAlignment = Alignment.CenterVertically) {
        IconButton(onClick = onBack) {
            Icon(painterResource(R.drawable.ic_sl_arrow_back), contentDescription = "Back")
        }
        Spacer(Modifier.width(8.dp))
        Text(title, style = MaterialTheme.typography.titleLarge)
    }
}

@Composable
fun Heading(text: String) {
    Text(
        text,
        color = MaterialTheme.colorScheme.tertiary,
        fontWeight = FontWeight.Medium,
        fontSize = 14.sp,
        modifier = Modifier.padding(start = 16.dp, end = 16.dp, top = 20.dp, bottom = 8.dp),
    )
}

/** A filled card, extra-large rounded. */
@Composable
fun SectionCard(
    color: Color,
    modifier: Modifier = Modifier,
    padding: PaddingValues = PaddingValues(vertical = 4.dp),
    content: @Composable ColumnScope.() -> Unit,
) {
    Column(
        modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(28.dp))
            .background(color)
            .padding(padding),
        content = content,
    )
}

/** What leads a [ListRow]: a glyph in a tonal circle, or an app's own icon. */
sealed interface Leading {
    data class Glyph(val res: Int) : Leading
    data class Picture(val drawable: Drawable?, val tinted: Boolean = false) : Leading
}

/** A list row: a leading icon, a title over a supporting line, and an optional trailing part. */
@Composable
fun ListRow(
    leading: Leading,
    title: String,
    supporting: String? = null,
    titleSize: Int = 16,
    trailing: (@Composable () -> Unit)? = null,
    onClick: (() -> Unit)? = null,
) {
    Row(
        Modifier
            .fillMaxWidth()
            .heightIn(min = 72.dp)
            .then(if (onClick != null) Modifier.clickable(onClick = onClick) else Modifier)
            .padding(horizontal = 16.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        val colors = MaterialTheme.colorScheme
        when (leading) {
            is Leading.Glyph -> Badge { Icon(painterResource(leading.res), null, Modifier.size(24.dp)) }
            is Leading.Picture -> if (leading.tinted) {
                Badge {
                    DrawableImage(leading.drawable, Modifier.size(24.dp), tint = colors.onPrimaryContainer)
                }
            } else {
                DrawableImage(leading.drawable, Modifier.size(40.dp))
            }
        }
        Column(Modifier.weight(1f).padding(start = 16.dp, end = 8.dp)) {
            Text(title, color = colors.onSurface, fontSize = titleSize.sp)
            if (supporting != null) {
                Text(supporting, color = colors.onSurfaceVariant, fontSize = 14.sp)
            }
        }
        trailing?.invoke()
    }
}

@Composable
private fun Badge(content: @Composable () -> Unit) {
    Box(
        Modifier
            .size(40.dp)
            .clip(CircleShape)
            .background(MaterialTheme.colorScheme.primaryContainer),
        contentAlignment = Alignment.Center,
    ) {
        androidx.compose.runtime.CompositionLocalProvider(
            androidx.compose.material3.LocalContentColor provides
                MaterialTheme.colorScheme.onPrimaryContainer,
            content = content,
        )
    }
}

@Composable
fun Chevron() {
    Icon(
        painterResource(R.drawable.ic_sl_chevron),
        contentDescription = null,
        tint = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

/** A row's switch: the row itself takes the tap. */
@Composable
fun RowSwitch(checked: Boolean) {
    Switch(
        checked = checked,
        onCheckedChange = null,
        colors = SwitchDefaults.colors(
            checkedThumbColor = MaterialTheme.colorScheme.onTertiary,
            checkedTrackColor = MaterialTheme.colorScheme.tertiary,
        ),
    )
}

@Composable
fun FilledButton(label: String, onClick: () -> Unit) {
    Button(onClick = onClick, contentPadding = PaddingValues(horizontal = 24.dp)) { Text(label) }
}

@Composable
fun PlainButton(label: String, onClick: () -> Unit) {
    TextButton(onClick = onClick) { Text(label) }
}

/** A titled list of options, one picked; picking closes it. */
@Composable
fun ChoiceDialog(
    title: String,
    options: List<String>,
    selected: Int,
    onDismiss: () -> Unit,
    onPick: (Int) -> Unit,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(title) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(0.dp)) {
                options.forEachIndexed { index, option ->
                    Row(
                        Modifier
                            .fillMaxWidth()
                            .heightIn(min = 56.dp)
                            .selectable(index == selected) { onPick(index) },
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        RadioButton(selected = index == selected, onClick = null)
                        Spacer(Modifier.width(16.dp))
                        Text(option)
                    }
                }
            }
        },
        confirmButton = {},
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}
