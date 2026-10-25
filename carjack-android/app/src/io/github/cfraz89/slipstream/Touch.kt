package io.github.cfraz89.slipstream

/** Where the accessory's touches go, in its panel's pixels, a pointer id per finger. */
fun interface Touch {
    fun touch(action: Int, pointer: Int, panelX: Float, panelY: Float)

    companion object {
        const val DOWN = 0
        const val UP = 1
        const val MOVE = 2
    }
}
