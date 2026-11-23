package io.github.cfraz89.slipstream.launcher

/** The launcher previewed on the phone, leaving on back: [onExit], set by the preview, closes it at once. */
object LauncherPreview {
    @Volatile var onExit: (() -> Unit)? = null
}
