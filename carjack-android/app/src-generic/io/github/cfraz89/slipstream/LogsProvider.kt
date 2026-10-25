package io.github.cfraz89.slipstream

import android.content.ContentProvider
import android.content.ContentValues
import android.database.Cursor
import android.database.MatrixCursor
import android.net.Uri
import android.os.ParcelFileDescriptor
import android.provider.OpenableColumns
import java.io.File
import java.io.FileNotFoundException

/** Hands a log bundle ([LogBundle]) to the app it's shared with, for that grant only. */
class LogsProvider : ContentProvider() {
    companion object {
        const val AUTHORITY = Variant.APP_ID + ".logs"

        fun uriFor(bundle: File): Uri = Uri.Builder().scheme("content").authority(AUTHORITY)
            .appendPath(bundle.name).build()
    }

    /** Only bundles, by name: no path out of the bundle directory. */
    private fun fileFor(uri: Uri): File {
        val name = uri.lastPathSegment
        if (name == null || name.contains("/") || !name.endsWith(".zip")) {
            throw FileNotFoundException(uri.toString())
        }
        val file = File(LogBundle.dir(context!!), name)
        if (!file.exists()) throw FileNotFoundException(uri.toString())
        return file
    }

    override fun onCreate() = true

    override fun openFile(uri: Uri, mode: String): ParcelFileDescriptor =
        ParcelFileDescriptor.open(fileFor(uri), ParcelFileDescriptor.MODE_READ_ONLY)

    override fun getType(uri: Uri) = "application/zip"

    override fun query(
        uri: Uri, projection: Array<String>?, selection: String?, args: Array<String>?,
        sort: String?,
    ): Cursor? {
        val file = try {
            fileFor(uri)
        } catch (e: FileNotFoundException) {
            return null
        }
        return MatrixCursor(arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE)).apply {
            addRow(arrayOf<Any>(file.name, file.length()))
        }
    }

    override fun insert(uri: Uri, values: ContentValues?): Uri? = null

    override fun delete(uri: Uri, selection: String?, args: Array<String>?) = 0

    override fun update(
        uri: Uri, values: ContentValues?, selection: String?, args: Array<String>?,
    ) = 0
}
