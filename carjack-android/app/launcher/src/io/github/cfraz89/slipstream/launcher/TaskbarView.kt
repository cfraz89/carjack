package io.github.cfraz89.slipstream.launcher

import android.content.Context
import android.util.AttributeSet
import android.view.LayoutInflater
import android.view.View
import android.widget.ImageView
import android.widget.LinearLayout

/**
 * All Apps, a divider, then the recents. Settings sits at the far end, mirroring the
 * way the tablet taskbar keeps its buttons away from the icon group.
 */
class TaskbarView @JvmOverloads constructor(
    context: Context,
    attrs: AttributeSet? = null,
    defStyleAttr: Int = 0,
) : LinearLayout(context, attrs, defStyleAttr) {

    var onAllApps: (() -> Unit)? = null
    var onSettings: (() -> Unit)? = null

    private val recentsContainer: LinearLayout
    private val divider: View

    init {
        orientation = HORIZONTAL
        LayoutInflater.from(context).inflate(R.layout.view_taskbar, this, true)
        recentsContainer = findViewById(R.id.taskbar_recents)
        divider = findViewById(R.id.taskbar_divider)
        findViewById<View>(R.id.taskbar_all_apps).setOnClickListener { onAllApps?.invoke() }
        findViewById<View>(R.id.taskbar_settings).setOnClickListener { onSettings?.invoke() }
        // Clipped, so the blur follows the pill rather than filling the bounding box.
        clipToOutline = true
        Blur.apply(this)
    }

    fun refresh() {
        recentsContainer.removeAllViews()
        val entries = RecentsStore.visible(context, Prefs.recentCount(context))
        divider.visibility = if (entries.isEmpty()) GONE else VISIBLE

        val inflater = LayoutInflater.from(context)
        for (entry in entries) {
            val icon = inflater.inflate(
                R.layout.item_taskbar_app,
                recentsContainer,
                false,
            ) as ImageView
            icon.setImageDrawable(entry.icon)
            icon.contentDescription = entry.label
            icon.setOnClickListener { AppRepository.launch(context, entry.component) }
            recentsContainer.addView(icon)
        }
    }
}
