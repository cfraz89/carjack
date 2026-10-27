package io.github.cfraz89.slipstream

import android.content.Intent
import android.graphics.drawable.Drawable
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Checkbox
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** The apps the switchgear goes into remote mode for while they're on the head unit ([Switchgear]). */
class RemoteAppsActivity : ComponentActivity() {
    private class App(val pkg: String, val label: String, val icon: Drawable)

    private var apps by mutableStateOf<List<App>?>(null)
    private var chosen by mutableStateOf(emptySet<String>())

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.setDecorFitsSystemWindows(false)
        chosen = Switchgear.apps(this)
        // Off the main thread: a phone's worth of labels and icons takes a moment.
        Thread({
            val found = load()
            runOnUiThread { if (!isDestroyed) apps = found }
        }, "slipstream-apps").start()
        setContent {
            SlipstreamTheme {
                Page {
                    TopBar("Remote mode apps") { finish() }
                    Text(
                        "The switchgear sends plain arrows while one of these is on the head unit, for " +
                            "apps that map a remote's keys themselves. A long press on right " +
                            "switches modes by hand.",
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        fontSize = 14.sp,
                        modifier = Modifier.padding(start = 16.dp, end = 16.dp, bottom = 8.dp),
                    )
                    Spacer(Modifier.height(4.dp))
                    val list = apps ?: return@Page
                    SectionCard(MaterialTheme.colorScheme.surfaceContainer) {
                        list.forEach { app ->
                            val on = app.pkg in chosen
                            ListRow(
                                Leading.Picture(app.icon),
                                app.label,
                                trailing = { Checkbox(checked = on, onCheckedChange = null) },
                            ) {
                                chosen = if (on) chosen - app.pkg else chosen + app.pkg
                                Switchgear.setApps(this@RemoteAppsActivity, chosen)
                            }
                        }
                    }
                }
            }
        }
    }

    /** Chosen ones first, then the rest by name. */
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
        }.sortedWith(compareBy<App>({ it.pkg !in chosen }, { it.label.lowercase() }))
    }
}
