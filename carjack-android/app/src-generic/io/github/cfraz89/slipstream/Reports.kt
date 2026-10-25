package io.github.cfraz89.slipstream

import android.content.Context
import android.util.Log
import io.sentry.Attachment
import io.sentry.Breadcrumb
import io.sentry.Hint
import io.sentry.Sentry
import io.sentry.SentryLevel
import io.sentry.SentryLogLevel
import io.sentry.SentryOptions
import io.sentry.android.core.SentryAndroid
import io.sentry.protocol.Feedback
import java.io.File
import java.io.IOException
import java.io.RandomAccessFile

/**
 * Crash and error reports, and sent logs, through Sentry: on unless turned off, and only with a
 * DSN built in (res/values/sentry.xml).
 */
object Reports {
    private const val TAG = "carjack"
    const val PREF = "crash_reports"
    /** Above any log bundle seen; a bigger one goes by email. */
    const val MAX_ATTACHMENT = 40L shl 20
    /** Of Slipstream's log, with each error: enough for the minutes before it. */
    private const val LOG_TAIL = 256 shl 10
    private const val PANIC_FLUSH_MS = 2000L

    /** Starts or stops reporting, to match the switch. */
    fun apply(context: Context) {
        val app = context.applicationContext
        if (!configured(app) || !Variant.prefs(app).getBoolean(PREF, true)) {
            Sentry.close()
            return
        }
        SentryAndroid.init(app) { options ->
            options.dsn = app.getString(R.string.sentry_dsn)
            options.isSendDefaultPii = false
            options.maxBreadcrumbs = 200
            options.maxAttachmentSize = MAX_ATTACHMENT
            options.logs.isEnabled = true
            options.beforeSend = SentryOptions.BeforeSendCallback { event, hint ->
                logTail(app)?.let { hint.addAttachment(Attachment(it, Variant.LOG_FILE, "text/plain")) }
                event
            }
        }
    }

    /** Whether this build has somewhere to send reports. */
    fun configured(context: Context) = context.getString(R.string.sentry_dsn).isNotEmpty()

    fun on() = Sentry.isEnabled()

    /** Info and warnings as logs and breadcrumbs; errors and panics as events too. */
    val CORE = Native.Reporter { priority, target, message ->
        if (!Sentry.isEnabled()) return@Reporter
        val level = when {
            priority >= 7 -> SentryLevel.FATAL
            priority == 6 -> SentryLevel.ERROR
            priority == 5 -> SentryLevel.WARNING
            else -> SentryLevel.INFO
        }
        Sentry.addBreadcrumb(
            Breadcrumb().apply {
                category = target
                this.message = message
                this.level = level
            },
        )
        val logLevel = when {
            priority >= 7 -> SentryLogLevel.FATAL
            priority == 6 -> SentryLogLevel.ERROR
            priority == 5 -> SentryLogLevel.WARN
            else -> SentryLogLevel.INFO
        }
        Sentry.logger().log(logLevel, "[$target] $message")
        if (priority >= 6) {
            Sentry.captureMessage(message, level) { scope -> scope.setTag("target", target) }
        }
        // The process aborts next.
        if (priority >= 7) Sentry.flush(PANIC_FLUSH_MS)
    }

    /** A log bundle as user feedback, with what the sender said happened. */
    fun sendLogs(bundle: File, description: String) {
        val feedback = Feedback(description.ifEmpty { "Logs sent" })
        Sentry.captureFeedback(
            feedback,
            Hint.withAttachment(Attachment(bundle.path, bundle.name, "application/zip")),
        )
    }

    private fun logTail(context: Context): ByteArray? {
        val dir = context.getExternalFilesDir(null) ?: return null
        return try {
            RandomAccessFile(File(dir, Variant.LOG_FILE), "r").use { log ->
                val start = maxOf(0L, log.length() - LOG_TAIL)
                val tail = ByteArray((log.length() - start).toInt())
                log.seek(start)
                log.readFully(tail)
                tail
            }
        } catch (e: IOException) {
            Log.w(TAG, "No log for the report", e)
            null
        }
    }
}
