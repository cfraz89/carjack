package io.github.cfraz89.slipstream

import android.bluetooth.BluetoothSocket
import android.content.Context
import android.os.Handler
import android.os.ParcelFileDescriptor
import android.util.Log
import java.io.FileInputStream
import java.io.FileOutputStream
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream

/** One CarPlay connection: the core's callbacks, answered with the platform's pieces. */
class Session(
    private val context: Context,
    private val handler: Handler,
    private val socket: BluetoothSocket,
    private val wifi: AccessoryWifi,
    /** Where the session is up to, for the notification. */
    private val status: (String) -> Unit,
    private val listener: (Session) -> Unit,
) : Native.Callbacks {
    private companion object {
        const val TAG = "carjack"
        const val KEYCODE_BACK = 4
        const val KEYCODE_DPAD_CENTER = 23
    }

    @Volatile private var handle = 0L
    private var stream: ScreenStream? = null
    @Volatile private var touch: Touch? = null
    /** Video got as far as starting. */
    @Volatile var streamed = false
        private set

    /** Relays the RFCOMM channel through a socket pair, since the core wants a plain fd. */
    fun start(name: String?, address: String?, codec: Int) {
        val pair = ParcelFileDescriptor.createSocketPair()
        val ours = pair[0]
        pump("carjack-iap-rx", socket.inputStream, FileOutputStream(ours.fileDescriptor), ours)
        pump("carjack-iap-tx", FileInputStream(ours.fileDescriptor), socket.outputStream, ours)
        handle = Native.startSession(this, pair[1].detachFd(), name, address, codec)
    }

    private fun pump(name: String, input: InputStream, out: OutputStream, ours: ParcelFileDescriptor) {
        val thread = Thread({
            val buf = ByteArray(4096)
            try {
                while (true) {
                    val n = input.read(buf)
                    if (n <= 0) break
                    out.write(buf, 0, n)
                }
            } catch (ignored: IOException) {
            }
            Bluetooth.closeQuietly(socket)
            try {
                ours.close()
            } catch (ignored: IOException) {
            }
        }, name)
        thread.isDaemon = true
        thread.start()
    }

    fun stop() {
        Native.stopSession(handle)
    }

    override fun joinWifi(ssid: String, passphrase: String?, bssid: String?) {
        status("Joining $ssid")
        handler.post {
            wifi.join(ssid, passphrase, bssid, object : AccessoryWifi.Result {
                override fun joined(iface: String, mac: String) {
                    status("Starting the stream")
                    Native.wifiJoined(handle, iface, mac)
                }

                override fun failed(reason: String) {
                    status("Couldn't join $ssid: $reason")
                    Native.wifiJoined(handle, null, reason)
                }
            })
        }
    }

    override fun startVideo(
        width: Int, height: Int, fps: Int, viewX: Int, viewY: Int, viewWidth: Int,
        viewHeight: Int, hevc: Boolean,
    ) {
        streamed = true
        status("Streaming at ${width}x$height")
        handler.post {
            // The back and home bar, where wanted, takes the bottom of the view from the display.
            val bar = NavBar.height(context, viewHeight)
            val displayHeight = viewHeight - bar
            val display = Variant.touch(context, viewX, viewY, viewWidth, displayHeight)
            touch = if (bar == 0) {
                display
            } else {
                NavBar.touch(
                    display, viewX, viewY + displayHeight, viewWidth, bar,
                    { Variant.key(context, NavBar.KEYCODE_BACK) },
                    { Variant.goHome(context) },
                )
            }
            val s = ScreenStream(context, handle) { stop() }
            stream = s
            try {
                s.start(width, height, fps, viewX, viewY, viewWidth, displayHeight, bar, hevc)
            } catch (e: IOException) {
                Log.e(TAG, "Could not start the screen stream", e)
                stop()
            } catch (e: RuntimeException) {
                Log.e(TAG, "Could not start the screen stream", e)
                stop()
            }
        }
    }

    override fun requestKeyframe() {
        stream?.requestKeyframe()
    }

    override fun touch(action: Int, pointer: Int, x: Float, y: Float) {
        touch?.touch(action, pointer, x, y)
    }

    override fun key(code: Int, down: Boolean) {
        val s = stream
        if (down && s != null && (code == KEYCODE_BACK || code == KEYCODE_DPAD_CENTER)) {
            s.cue(code == KEYCODE_BACK)
        }
        Variant.key(context, code, down)
    }

    override fun ended(error: String?) {
        Log.i(TAG, "Session ended" + if (error == null) "" else ": $error")
        status("Session ended" + (if (error == null) "" else " ($error)") + "; waiting for the unit")
        handler.post {
            stream?.stop()
            stream = null
            touch = null
            wifi.leave()
            Bluetooth.closeQuietly(socket)
            listener(this)
        }
    }
}
