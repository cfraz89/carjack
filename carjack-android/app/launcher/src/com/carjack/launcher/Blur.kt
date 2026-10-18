package com.carjack.launcher

import android.graphics.RenderEffect
import android.graphics.Shader
import android.util.Log
import android.view.View

/**
 * Backdrop blur for the panels over the wallpaper.
 *
 * setBackdropRenderEffect is @hide, reachable by reflection only where hidden APIs are allowed (a
 * system app); elsewhere the panels stay flat. It snapshots this window's own surface, which is
 * why HomeActivity draws the wallpaper itself instead of using windowShowWallpaper - a wallpaper
 * behind the window is a separate surface and never reaches the backdrop.
 */
object Blur {
    private const val TAG = "CarjackBlur"

    private val setBackdropRenderEffect = try {
        View::class.java.getMethod("setBackdropRenderEffect", RenderEffect::class.java)
    } catch (e: Throwable) {
        Log.w(TAG, "no backdrop blur; panels stay flat", e)
        null
    }

    /** MIRROR rather than CLAMP: a clamped edge smears the panel's border colour outwards. */
    fun apply(view: View, radius: Float) = set(
        view,
        RenderEffect.createBlurEffect(radius, radius, Shader.TileMode.MIRROR),
    )

    fun clear(view: View) = set(view, null)

    private fun set(view: View, effect: RenderEffect?) {
        val method = setBackdropRenderEffect ?: return
        try {
            method.invoke(view, effect)
        } catch (e: Throwable) {
            Log.w(TAG, "backdrop blur failed", e)
        }
    }
}
