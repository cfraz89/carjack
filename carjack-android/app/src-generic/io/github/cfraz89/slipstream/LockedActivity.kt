package io.github.cfraz89.slipstream

import android.app.KeyguardManager
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.size
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import java.lang.ref.WeakReference

/**
 * The head unit's screen while the phone is locked ([LockWatcher]): nothing else on it can draw, so it
 * says why rather than showing a launcher of black apps. Closes itself at unlock.
 */
class LockedActivity : ComponentActivity() {
    companion object {
        private var current = WeakReference<LockedActivity>(null)

        fun close() {
            current.get()?.let { it.runOnUiThread { it.finish() } }
        }
    }

    private val unlocked = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) = finish()
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        current = WeakReference(this)
        registerReceiver(unlocked, IntentFilter(Intent.ACTION_USER_PRESENT))
        setContent {
            Column(
                Modifier.fillMaxSize().background(Color(0xFF111318)),
                verticalArrangement = Arrangement.Center,
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                Icon(
                    painterResource(R.drawable.ic_sl_lock),
                    contentDescription = null,
                    tint = Color(0xFFA5C8FF),
                    modifier = Modifier.size(96.dp),
                )
                Spacer(Modifier.height(24.dp))
                Text("Phone locked", color = Color(0xFFE2E2E9), fontSize = 40.sp, fontWeight = FontWeight.Medium)
                Spacer(Modifier.height(8.dp))
                Text("Unlock it to continue", color = Color(0xFFC3C6CF), fontSize = 24.sp)
            }
        }
    }

    override fun onResume() {
        super.onResume()
        if (!getSystemService(KeyguardManager::class.java).isKeyguardLocked) finish()
    }

    override fun onDestroy() {
        unregisterReceiver(unlocked)
        super.onDestroy()
    }
}
