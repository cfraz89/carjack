package io.github.cfraz89.slipstream

import android.content.Intent
import android.graphics.drawable.Drawable
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.height
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.unit.dp

/** Every app with a launcher icon, by name, for the unit to open on connect. */
class AppPickerActivity : ComponentActivity() {
    companion object {
        const val EXTRA_PACKAGE = "package"
    }

    private class App(val pkg: String, val label: String, val icon: Drawable)

    private var apps by mutableStateOf<List<App>?>(null)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.setDecorFitsSystemWindows(false)
        // Off the main thread: a phone's worth of labels and icons takes a moment.
        Thread({
            val found = load()
            runOnUiThread { if (!isDestroyed) apps = found }
        }, "slipstream-apps").start()
        val current = Choices.current(this).split("/")[0]
        setContent {
            SlipstreamTheme {
                Page {
                    TopBar("Choose an app") { finish() }
                    Spacer(Modifier.height(12.dp))
                    val list = apps ?: return@Page
                    SectionCard(MaterialTheme.colorScheme.surfaceContainer) {
                        list.forEach { app ->
                            ListRow(
                                Leading.Picture(app.icon),
                                app.label,
                                trailing = if (app.pkg == current) {
                                    {
                                        Icon(
                                            painterResource(R.drawable.ic_sl_check), null,
                                            tint = MaterialTheme.colorScheme.primary,
                                        )
                                    }
                                } else {
                                    null
                                },
                            ) {
                                setResult(RESULT_OK, Intent().putExtra(EXTRA_PACKAGE, app.pkg))
                                finish()
                            }
                        }
                    }
                }
            }
        }
    }

    private fun load(): List<App> {
        val pm = packageManager
        val seen = mutableSetOf<String>()
        return pm.queryIntentActivities(
            Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER), 0,
        ).mapNotNull { info ->
            val app = info.activityInfo.applicationInfo
            if (app.packageName == packageName || !seen.add(app.packageName)) {
                null
            } else {
                App(app.packageName, app.loadLabel(pm).toString(), app.loadIcon(pm))
            }
        }.sortedBy { it.label.lowercase() }
    }
}
