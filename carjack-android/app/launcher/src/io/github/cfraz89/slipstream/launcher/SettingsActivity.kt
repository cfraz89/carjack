package io.github.cfraz89.slipstream.launcher

import android.content.Intent
import android.os.Bundle
import android.provider.MediaStore
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.systemBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.res.dimensionResource
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import io.github.cfraz89.slipstream.launcher.Blur.backdrop

/** Wallpaper, grid dimensions, and how many recents the taskbar carries. */
class SettingsActivity : ComponentActivity() {

    private var wallpaper by mutableStateOf(Wallpapers.PHONE)
    private var choosingWallpaper by mutableStateOf(false)
    /** Bumped on each stepper change, so the rows read their values again. */
    private var revision by mutableIntStateOf(0)

    /** The system photo picker, which needs no storage permission. */
    private val photo = registerForActivityResult(ActivityResultContracts.StartActivityForResult()) {
        val uri = it.data?.data
        if (it.resultCode != RESULT_OK || uri == null) return@registerForActivityResult
        Thread({
            Wallpapers.savePhoto(this, uri)
            runOnUiThread { if (!isDestroyed) wallpaper = Wallpapers.choice(this) }
        }, "slipstream-photo").start()
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.setDecorFitsSystemWindows(false)
        wallpaper = Wallpapers.choice(this)
        setContent { LauncherTheme { Settings() } }
    }

    override fun onResume() {
        super.onResume()
        SystemBars.hide(this)
    }

    @Composable
    private fun Settings() {
        Column(
            Modifier
                .fillMaxSize()
                .backdrop()
                .background(Palette.sheet)
                .systemBarsPadding(),
        ) {
            // M3's small top app bar, taller than its 64dp for gloves.
            Row(
                Modifier
                    .fillMaxWidth()
                    .height(dimensionResource(R.dimen.top_app_bar_height))
                    .padding(
                        start = dimensionResource(R.dimen.top_app_bar_side_padding),
                        end = dimensionResource(R.dimen.grid_padding),
                    ),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Image(
                    painterResource(R.drawable.arrow_back_24),
                    contentDescription = stringResource(R.string.back),
                    modifier = Modifier
                        .size(dimensionResource(R.dimen.top_app_bar_nav_size))
                        .tappable(CircleShape) { finish() }
                        .padding(dimensionResource(R.dimen.icon_button_padding)),
                )
                Spacer(Modifier.width(dimensionResource(R.dimen.top_app_bar_title_gap)))
                Text(
                    stringResource(R.string.settings),
                    style = titleStyle(),
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            Column(
                Modifier
                    .weight(1f)
                    .verticalScroll(rememberScrollState())
                    .padding(
                        start = dimensionResource(R.dimen.grid_padding),
                        end = dimensionResource(R.dimen.grid_padding),
                        bottom = dimensionResource(R.dimen.grid_padding),
                    ),
                verticalArrangement = Arrangement.spacedBy(dimensionResource(R.dimen.setting_row_gap)),
            ) {
                WallpaperRow()
                revision.let {
                    Stepper(
                        R.string.setting_columns, Prefs.MIN_COLUMNS, Prefs.MAX_COLUMNS,
                        Prefs.columns(this@SettingsActivity),
                    ) { Prefs.setColumns(this@SettingsActivity, it) }
                    Stepper(
                        R.string.setting_rows, Prefs.MIN_ROWS, Prefs.MAX_ROWS,
                        Prefs.rows(this@SettingsActivity),
                    ) { Prefs.setRows(this@SettingsActivity, it) }
                    Stepper(
                        R.string.setting_recents, Prefs.MIN_RECENTS, Prefs.MAX_RECENTS,
                        Prefs.recentCount(this@SettingsActivity),
                    ) { Prefs.setRecentCount(this@SettingsActivity, it) }
                }
            }
        }
        if (choosingWallpaper) WallpaperDialog()
    }

    @Composable
    private fun WallpaperRow() {
        Column(
            Modifier
                .fillMaxWidth()
                .heightIn(min = dimensionResource(R.dimen.stepper_button_size))
                .clip(CardShape)
                .background(Palette.card)
                .tappable(CardShape) { choosingWallpaper = true }
                .padding(
                    horizontal = dimensionResource(R.dimen.setting_row_padding),
                    vertical = dimensionResource(R.dimen.shortcut_padding),
                ),
            verticalArrangement = Arrangement.Center,
        ) {
            Text(
                stringResource(R.string.setting_wallpaper),
                color = Palette.text,
                fontSize = sp(R.dimen.setting_label_size),
            )
            Text(
                stringResource(wallpaperName(wallpaper)),
                color = Palette.textSecondary,
                fontSize = sp(R.dimen.app_label_size),
            )
        }
    }

    private fun wallpaperName(choice: String) = when (choice) {
        Wallpapers.BUILT_IN -> R.string.wallpaper_builtin
        Wallpapers.PHOTO -> R.string.wallpaper_photo
        else -> R.string.wallpaper_phone
    }

    @Composable
    private fun WallpaperDialog() {
        val options = listOf(
            Wallpapers.PHONE to R.string.wallpaper_phone,
            Wallpapers.BUILT_IN to R.string.wallpaper_builtin,
            Wallpapers.PHOTO to R.string.wallpaper_pick_photo,
        )
        AlertDialog(
            onDismissRequest = { choosingWallpaper = false },
            title = { Text(stringResource(R.string.setting_wallpaper)) },
            text = {
                Column {
                    options.forEach { (choice, label) ->
                        Row(
                            Modifier
                                .fillMaxWidth()
                                .heightIn(min = 56.dp)
                                .selectable(wallpaper == choice, role = Role.RadioButton) {
                                    choosingWallpaper = false
                                    if (choice == Wallpapers.PHOTO) {
                                        photo.launch(
                                            Intent(MediaStore.ACTION_PICK_IMAGES).setType("image/*"),
                                        )
                                    } else {
                                        Wallpapers.choose(this@SettingsActivity, choice)
                                        wallpaper = choice
                                    }
                                },
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            RadioButton(selected = wallpaper == choice, onClick = null)
                            Spacer(Modifier.width(16.dp))
                            Text(stringResource(label))
                        }
                    }
                }
            },
            confirmButton = {},
        )
    }

    /** A setting's label, then − value +. */
    @Composable
    private fun Stepper(label: Int, min: Int, max: Int, value: Int, write: (Int) -> Unit) {
        Row(
            Modifier
                .fillMaxWidth()
                .clip(CardShape)
                .background(Palette.card)
                .padding(
                    start = dimensionResource(R.dimen.setting_row_padding),
                    end = dimensionResource(R.dimen.shortcut_padding),
                    top = dimensionResource(R.dimen.shortcut_padding),
                    bottom = dimensionResource(R.dimen.shortcut_padding),
                ),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                stringResource(label),
                color = Palette.text,
                fontSize = sp(R.dimen.setting_label_size),
                modifier = Modifier.weight(1f),
            )
            StepButton("−", R.string.decrease, value > min) {
                write((value - 1).coerceAtLeast(min))
                revision++
            }
            Text(
                value.toString(),
                color = Palette.text,
                fontSize = sp(R.dimen.stepper_value_size),
                textAlign = TextAlign.Center,
                modifier = Modifier.width(72.dp),
            )
            StepButton("+", R.string.increase, value < max) {
                write((value + 1).coerceAtMost(max))
                revision++
            }
        }
    }

    @Composable
    private fun StepButton(symbol: String, label: Int, enabled: Boolean, onClick: () -> Unit) {
        val description = stringResource(label)
        Box(
            Modifier
                .size(dimensionResource(R.dimen.stepper_button_size))
                .alpha(if (enabled) 1f else 0.3f)
                .clip(CircleShape)
                .background(Palette.tonal)
                .then(if (enabled) Modifier.tappable(CircleShape, onClick = onClick) else Modifier),
            contentAlignment = Alignment.Center,
        ) {
            Text(
                symbol,
                color = Palette.onTonal,
                fontSize = sp(R.dimen.stepper_value_size),
                modifier = Modifier.semantics { contentDescription = description },
            )
        }
    }
}

@Composable
private fun titleStyle() = TextStyle(
    fontFamily = FontFamily.SansSerif,
    fontWeight = FontWeight.Medium,
    fontSize = sp(R.dimen.screen_title_size),
    color = Palette.text,
)
