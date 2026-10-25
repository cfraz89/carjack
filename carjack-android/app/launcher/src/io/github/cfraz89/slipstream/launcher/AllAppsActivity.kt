package io.github.cfraz89.slipstream.launcher

import android.content.Intent
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.asPaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.res.dimensionResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import io.github.cfraz89.slipstream.launcher.Blur.backdrop

/**
 * Every launchable app, as a bottom sheet over home. Doubles as the slot picker: in pick mode a
 * tap returns the chosen component instead of launching it.
 */
class AllAppsActivity : ComponentActivity() {

    companion object {
        const val EXTRA_PICK_MODE = "io.github.cfraz89.slipstream.launcher.PICK_MODE"
        const val EXTRA_COMPONENT = "io.github.cfraz89.slipstream.launcher.COMPONENT"
    }

    private var pickMode = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.setDecorFitsSystemWindows(false)

        // Up from the bottom and back down, with home held still underneath.
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            overrideActivityTransition(OVERRIDE_TRANSITION_OPEN, R.anim.sheet_enter, R.anim.hold)
            overrideActivityTransition(OVERRIDE_TRANSITION_CLOSE, R.anim.hold, R.anim.sheet_exit)
        }

        pickMode = intent.getBooleanExtra(EXTRA_PICK_MODE, false)
        val apps = AppRepository.launchableApps(this)
        setContent { LauncherTheme { Sheet(apps) } }
    }

    override fun onResume() {
        super.onResume()
        SystemBars.hide(this)
    }

    private fun choose(entry: AppEntry) {
        if (pickMode) {
            setResult(
                RESULT_OK,
                Intent().putExtra(EXTRA_COMPONENT, entry.component.flattenToString()),
            )
            finish()
        } else if (AppRepository.launch(this, entry.component)) {
            // Otherwise the drawer is still open over home when the app is left.
            finish()
        }
    }

    /**
     * The window is edge to edge, so the sheet runs to the bottom edge behind the navigation bar,
     * and only its top and the grid's scroll padding step clear of the bars.
     */
    @Composable
    private fun Sheet(apps: List<AppEntry>) {
        val corner = dimensionResource(R.dimen.sheet_corner_radius)
        val shape = RoundedCornerShape(topStart = corner, topEnd = corner)
        Column(
            Modifier
                .fillMaxSize()
                .statusBarsPadding()
                .padding(top = dimensionResource(R.dimen.sheet_top_gap))
                .clip(shape)
                .backdrop()
                .background(Palette.sheet),
        ) {
            // The handle and title close the sheet: no scrim shows above it to tap.
            Column(Modifier.fillMaxWidth().tappable(RoundedCornerShape(0.dp)) { finish() }) {
                Box(
                    Modifier
                        .padding(top = dimensionResource(R.dimen.handle_margin))
                        .size(
                            dimensionResource(R.dimen.handle_width),
                            dimensionResource(R.dimen.handle_height),
                        )
                        .background(Palette.textSecondary, RoundedCornerShape(50))
                        .align(Alignment.CenterHorizontally),
                )
                Text(
                    stringResource(if (pickMode) R.string.pick_an_app else R.string.all_apps),
                    style = TextStyle(
                        fontFamily = FontFamily.SansSerif,
                        fontWeight = FontWeight.Medium,
                        fontSize = sp(R.dimen.screen_title_size),
                        color = Palette.text,
                    ),
                    modifier = Modifier.padding(
                        horizontal = dimensionResource(R.dimen.grid_padding),
                        vertical = dimensionResource(R.dimen.shortcut_padding),
                    ),
                )
            }
            val pad = dimensionResource(R.dimen.shortcut_padding)
            val bottom = WindowInsets.navigationBars.asPaddingValues().calculateBottomPadding()
            // Adaptive, so the column count follows the head unit rather than being pinned.
            LazyVerticalGrid(
                columns = GridCells.Adaptive(dimensionResource(R.dimen.app_cell_width)),
                contentPadding = PaddingValues(pad, pad, pad, pad + bottom),
                modifier = Modifier.weight(1f),
            ) {
                items(apps, key = { it.component.flattenToString() }) { entry -> App(entry) }
            }
        }
    }

    @Composable
    private fun App(entry: AppEntry) {
        Column(
            Modifier
                .tappable(CardShape) { choose(entry) }
                .padding(dimensionResource(R.dimen.app_cell_padding)),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            DrawableImage(entry.icon, Modifier.size(dimensionResource(R.dimen.app_icon_size)))
            Spacer(Modifier.height(dimensionResource(R.dimen.shortcut_label_top)))
            Text(
                entry.label,
                color = Palette.text,
                fontSize = sp(R.dimen.app_label_size),
                textAlign = TextAlign.Center,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.fillMaxWidth(),
            )
        }
    }
}
