package io.github.cfraz89.slipstream.launcher

import android.app.Activity
import android.os.Bundle
import android.view.View
import android.widget.TextView

/** Grid dimensions, and how many recents the taskbar carries. */
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
    }

    override fun onResume() {
        super.onResume()
        SystemBars.hide(this)
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
