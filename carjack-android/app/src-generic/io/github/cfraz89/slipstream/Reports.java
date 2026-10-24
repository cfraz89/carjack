package io.github.cfraz89.slipstream;

import android.content.Context;
import android.util.Log;

import java.io.File;
import java.io.IOException;
import java.io.RandomAccessFile;

import io.sentry.Attachment;
import io.sentry.Breadcrumb;
import io.sentry.Hint;
import io.sentry.Sentry;
import io.sentry.SentryLevel;
import io.sentry.SentryLogLevel;
import io.sentry.android.core.SentryAndroid;
import io.sentry.protocol.Feedback;

/**
 * Crash and error reports, and sent logs, through Sentry: on unless turned off, and only with a
 * DSN built in (res/values/sentry.xml).
 */
final class Reports {
    private static final String TAG = "carjack";
    static final String PREF = "crash_reports";
    /** Above any log bundle seen; a bigger one goes by email. */
    static final long MAX_ATTACHMENT = 40L << 20;
    /** Of Slipstream's log, with each error: enough for the minutes before it. */
    private static final int LOG_TAIL = 256 << 10;
    private static final long PANIC_FLUSH_MS = 2000;

    /** Starts or stops reporting, to match the switch. */
    static void apply(Context context) {
        Context app = context.getApplicationContext();
        if (!configured(app) || !Variant.prefs(app).getBoolean(PREF, true)) {
            Sentry.close();
            return;
        }
        SentryAndroid.init(app, options -> {
            options.setDsn(app.getString(R.string.sentry_dsn));
            options.setSendDefaultPii(false);
            options.setMaxBreadcrumbs(200);
            options.setMaxAttachmentSize(MAX_ATTACHMENT);
            options.getLogs().setEnabled(true);
            options.setBeforeSend((event, hint) -> {
                byte[] tail = logTail(app);
                if (tail != null) {
                    hint.addAttachment(new Attachment(tail, Variant.LOG_FILE, "text/plain"));
                }
                return event;
            });
        });
    }

    /** Whether this build has somewhere to send reports. */
    static boolean configured(Context context) {
        return !context.getString(R.string.sentry_dsn).isEmpty();
    }

    static boolean on() {
        return Sentry.isEnabled();
    }

    /** Info and warnings as logs and breadcrumbs; errors and panics as events too. */
    static final Native.Reporter CORE = (priority, target, message) -> {
        if (!Sentry.isEnabled()) {
            return;
        }
        SentryLevel level = priority >= 7 ? SentryLevel.FATAL
                : priority == 6 ? SentryLevel.ERROR
                : priority == 5 ? SentryLevel.WARNING : SentryLevel.INFO;
        Breadcrumb crumb = new Breadcrumb();
        crumb.setCategory(target);
        crumb.setMessage(message);
        crumb.setLevel(level);
        Sentry.addBreadcrumb(crumb);
        SentryLogLevel logLevel = priority >= 7 ? SentryLogLevel.FATAL
                : priority == 6 ? SentryLogLevel.ERROR
                : priority == 5 ? SentryLogLevel.WARN : SentryLogLevel.INFO;
        Sentry.logger().log(logLevel, "[" + target + "] " + message);
        if (priority >= 6) {
            Sentry.captureMessage(message, level, scope -> scope.setTag("target", target));
        }
        // The process aborts next.
        if (priority >= 7) {
            Sentry.flush(PANIC_FLUSH_MS);
        }
    };

    /** A log bundle as user feedback, with what the sender said happened. */
    static void sendLogs(File bundle, String description) {
        Feedback feedback = new Feedback(description.isEmpty() ? "Logs sent" : description);
        Sentry.captureFeedback(feedback, Hint.withAttachment(
                new Attachment(bundle.getPath(), bundle.getName(), "application/zip")));
    }

    private static byte[] logTail(Context context) {
        File dir = context.getExternalFilesDir(null);
        if (dir == null) {
            return null;
        }
        try (RandomAccessFile log = new RandomAccessFile(new File(dir, Variant.LOG_FILE), "r")) {
            long start = Math.max(0, log.length() - LOG_TAIL);
            byte[] tail = new byte[(int) (log.length() - start)];
            log.seek(start);
            log.readFully(tail);
            return tail;
        } catch (IOException e) {
            Log.w(TAG, "No log for the report", e);
            return null;
        }
    }

    private Reports() {}
}
