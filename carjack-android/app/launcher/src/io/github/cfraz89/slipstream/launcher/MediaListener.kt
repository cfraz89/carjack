package io.github.cfraz89.slipstream.launcher

import android.service.notification.NotificationListenerService

/**
 * Does nothing itself: being an enabled notification listener is what lets [NowPlaying] see other
 * apps' media sessions. Enabled through Shizuku rather than asked for.
 */
class MediaListener : NotificationListenerService()
