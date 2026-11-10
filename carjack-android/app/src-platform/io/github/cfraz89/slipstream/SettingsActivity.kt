package io.github.cfraz89.slipstream

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.setValue

/**
 * The board's settings, opened on the head unit itself: those of Slipstream's that apply where the
 * board is the phone. The system bar already has back and home, so there's no bar of carjack's own.
 */
class SettingsActivity : ComponentActivity() {
    /** Bumped to read everything afresh from settings. */
    private var revision by mutableIntStateOf(0)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.setDecorFitsSystemWindows(false)
        setContent {
            SlipstreamTheme {
                key(revision) {
                    Page {
                        TopBar("Motopod") { finish() }
                        Heading("Connection")
                        SectionCard(MaterialTheme.colorScheme.surfaceContainer) {
                            HeadUnitRow { revision++ }
                            OnConnectRow()
                            ScaleRow { revision++ }
                            RemoteAppsRow()
                        }
                    }
                }
            }
        }
    }

    /** Back from a choice's own screen. */
    override fun onResume() {
        super.onResume()
        revision++
    }
}
