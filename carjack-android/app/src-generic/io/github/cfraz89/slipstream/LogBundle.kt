package io.github.cfraz89.slipstream

import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.ParcelFileDescriptor
import android.util.Log
import java.io.File
import java.io.FileOutputStream
import java.io.InputStream
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import java.util.zip.ZipEntry
import java.util.zip.ZipOutputStream

/**
 * Everything a report needs, zipped for sending: Slipstream's own log, the phone's system log
 * (which only Shizuku's shell can reach), and what phone and settings it ran on.
 */
object LogBundle {
    private const val TAG = "carjack"
    /** Where reports go; the share sheet still lets the sender pick another. */
    const val REPORT_EMAIL = "cfraz89@gmail.com"
    /** The system log's file suffixes, by [ShizukuDisplay.systemLog] part. */
    private val SYSTEM_LOG_PARTS = listOf("", ".1", ".2", ".3")

    fun dir(context: Context) = File(context.cacheDir, "logs")

    /**
     * Builds a bundle and sends it through [Reports], returning null; or, with reports off or a
     * bundle too big for them, returns the share sheet to send it with. Blocks: not on the UI
     * thread.
     */
    fun send(context: Context, description: String, email: Boolean): Intent? {
        val out = build(context)
        if (!email && Reports.on() && out.length() <= Reports.MAX_ATTACHMENT) {
            Reports.sendLogs(out, description)
            return null
        }
        val send = Intent(Intent.ACTION_SEND)
            .setType("application/zip")
            .putExtra(Intent.EXTRA_STREAM, LogsProvider.uriFor(out))
            .putExtra(Intent.EXTRA_EMAIL, arrayOf(REPORT_EMAIL))
            .putExtra(Intent.EXTRA_SUBJECT, "Slipstream logs")
            .putExtra(Intent.EXTRA_TEXT, "What happened, and roughly when:\n\n$description")
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        return Intent.createChooser(send, "Send logs")
    }

    private fun build(context: Context): File {
        val dir = dir(context)
        dir.mkdirs()
        // One bundle at a time: they're tens of megabytes.
        dir.listFiles()?.forEach { it.delete() }
        val external = context.getExternalFilesDir(null)
        val stamp = SimpleDateFormat("yyyyMMdd-HHmmss", Locale.ROOT).format(Date())
        val out = File(dir, "slipstream-logs-$stamp.zip")
        ZipOutputStream(FileOutputStream(out)).use { zip ->
            add(zip, "info.txt", info(context).toByteArray())
            external?.listFiles { _, name -> name.startsWith(Variant.LOG_FILE) }?.forEach { file ->
                file.inputStream().use { addLog(zip, file.name, it) }
            }
            // Read over Shizuku, not copied here by its shell: MediaProvider aborts on that.
            SYSTEM_LOG_PARTS.forEachIndexed { part, suffix ->
                ShizukuDisplay.systemLog(part)?.let { log ->
                    ParcelFileDescriptor.AutoCloseInputStream(log).use {
                        addLog(zip, "slipstream-system.log$suffix", it)
                    }
                }
            }
        }
        Log.i(TAG, "Log bundle of ${out.length()} bytes")
        return out
    }

    private fun addLog(zip: ZipOutputStream, name: String, log: InputStream) {
        zip.putNextEntry(ZipEntry(name))
        // Not closed: that would close the zip.
        val out = zip.writer()
        log.bufferedReader().lineSequence().filterNot(::secret).forEach {
            out.write(it)
            out.write('\n'.code)
        }
        out.flush()
        zip.closeEntry()
    }

    /** Lines that may carry the unit's WiFi password. */
    private fun secret(line: String) = "passphrase=" in line || "connect-network" in line

    private fun add(zip: ZipOutputStream, name: String, bytes: ByteArray) {
        zip.putNextEntry(ZipEntry(name))
        zip.write(bytes)
        zip.closeEntry()
    }

    private fun info(context: Context) = buildString {
        try {
            append("Slipstream ")
                .append(context.packageManager.getPackageInfo(context.packageName, 0).versionName)
                .append('\n')
        } catch (ignored: Exception) {
        }
        append(Build.MANUFACTURER).append(' ').append(Build.MODEL)
            .append(", Android ").append(Build.VERSION.RELEASE)
            .append(" (").append(Build.DISPLAY).append(")\n")
            .append("Shizuku: ").append(if (ShizukuDisplay.available()) "yes" else "no").append("\n\n")
            .append("Settings:\n")
        for ((key, value) in Variant.prefs(context).all) {
            append("  ").append(key).append(" = ").append(value).append('\n')
        }
    }
}
