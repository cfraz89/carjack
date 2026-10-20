package io.github.cfraz89.slipstream.launcher

import android.app.Activity
import android.app.AlertDialog
import android.content.Intent
import android.os.Bundle
import android.provider.MediaStore
import android.view.View
import android.widget.TextView

/** Wallpaper, grid dimensions, and how many recents the taskbar carries. */
class SettingsActivity : Activity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_settings)

        bindStepper(
            rowId = R.id.setting_columns,
            labelRes = R.string.setting_columns,
            min = Prefs.MIN_COLUMNS,
            max = Prefs.MAX_COLUMNS,
            read = { Prefs.columns(this) },
            write = { Prefs.setColumns(this, it) },
        )
        bindStepper(
            rowId = R.id.setting_rows,
            labelRes = R.string.setting_rows,
            min = Prefs.MIN_ROWS,
            max = Prefs.MAX_ROWS,
            read = { Prefs.rows(this) },
            write = { Prefs.setRows(this, it) },
        )
        bindStepper(
            rowId = R.id.setting_recents,
            labelRes = R.string.setting_recents,
            min = Prefs.MIN_RECENTS,
            max = Prefs.MAX_RECENTS,
            read = { Prefs.recentCount(this) },
            write = { Prefs.setRecentCount(this, it) },
        )

        findViewById<View>(R.id.settings_back).setOnClickListener { finish() }
        findViewById<View>(R.id.setting_wallpaper).setOnClickListener { chooseWallpaper() }
        showWallpaper()
    }

    override fun onResume() {
        super.onResume()
        SystemBars.hide(this)
    }

    private fun showWallpaper() {
        findViewById<TextView>(R.id.setting_wallpaper_value).setText(
            when (Wallpapers.choice(this)) {
                Wallpapers.BUILT_IN -> R.string.wallpaper_builtin
                Wallpapers.PHOTO -> R.string.wallpaper_photo
                else -> R.string.wallpaper_phone
            },
        )
    }

    private fun chooseWallpaper() {
        val options = arrayOf(
            getString(R.string.wallpaper_phone),
            getString(R.string.wallpaper_builtin),
            getString(R.string.wallpaper_pick_photo),
        )
        val current = when (Wallpapers.choice(this)) {
            Wallpapers.BUILT_IN -> 1
            Wallpapers.PHOTO -> 2
            else -> 0
        }
        AlertDialog.Builder(this, R.style.SlipstreamDialog)
            .setTitle(R.string.setting_wallpaper)
            .setSingleChoiceItems(options, current) { dialog, which ->
                dialog.dismiss()
                when (which) {
                    0 -> Wallpapers.choose(this, Wallpapers.PHONE)
                    1 -> Wallpapers.choose(this, Wallpapers.BUILT_IN)
                    // The system photo picker, which needs no storage permission.
                    else -> startActivityForResult(
                        Intent(MediaStore.ACTION_PICK_IMAGES).setType("image/*"),
                        REQUEST_PHOTO,
                    )
                }
                showWallpaper()
            }
            .show()
    }

    @Deprecated("Deprecated in Java")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        val uri = data?.data
        if (requestCode != REQUEST_PHOTO || resultCode != RESULT_OK || uri == null) return
        Thread({
            Wallpapers.savePhoto(this, uri)
            runOnUiThread { if (!isDestroyed) showWallpaper() }
        }, "slipstream-photo").start()
    }

    private companion object {
        const val REQUEST_PHOTO = 1
    }

    private fun bindStepper(
        rowId: Int,
        labelRes: Int,
        min: Int,
        max: Int,
        read: () -> Int,
        write: (Int) -> Unit,
    ) {
        val row = findViewById<View>(rowId)
        val value = row.findViewById<TextView>(R.id.stepper_value)
        val minus = row.findViewById<View>(R.id.stepper_minus)
        val plus = row.findViewById<View>(R.id.stepper_plus)
        row.findViewById<TextView>(R.id.stepper_label).setText(labelRes)

        fun render() {
            val current = read()
            value.text = current.toString()
            minus.isEnabled = current > min
            plus.isEnabled = current < max
            minus.alpha = if (minus.isEnabled) 1f else 0.3f
            plus.alpha = if (plus.isEnabled) 1f else 0.3f
        }

        minus.setOnClickListener {
            write((read() - 1).coerceAtLeast(min))
            render()
        }
        plus.setOnClickListener {
            write((read() + 1).coerceAtMost(max))
            render()
        }
        render()
    }
}
