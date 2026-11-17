package io.github.cfraz89.slipstream.aa

import android.util.Log

/** The receiver's log lines, under the app's tag and marked as Android Auto's. */
object AaLog {
    private const val TAG = "carjack"

    fun i(msg: String) = Log.i(TAG, "AA: $msg")
    fun w(msg: String) = Log.w(TAG, "AA: $msg")
    fun e(msg: String, e: Throwable? = null) = Log.e(TAG, "AA: $msg", e)
}
