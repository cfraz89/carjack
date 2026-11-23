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
    companion object {
        private const val TAG = "carjack"

        /** The session with video up, for settings that change its layout; null between them. */
        @Volatile var current: Session? = null
            private set
    }

    /** What [startVideo] was given, to build the stream again from. */
    private class Video(
        val width: Int, val height: Int, val fps: Int, val viewX: Int, val viewY: Int,
        val viewWidth: Int, val viewHeight: Int, val hevc: Boolean,
    )

    @Volatile private var handle = 0L
    private var video: Video? = null
    /** The [SystemBar]'s height in the stream as built; 0 for none. */
    private var bar = 0
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

    override fun identified(name: String?, model: String?, manufacturer: String?, vehicle: String?) {
        val bluetoothName = try {
            socket.remoteDevice.name
        } catch (e: SecurityException) {
            null
        }
        Profiles.connected(context, socket.remoteDevice.address, bluetoothName)
        Mappings.identified(context, HeadUnit(bluetoothName, name, model, manufacturer, vehicle))
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
        val v = Video(width, height, fps, viewX, viewY, viewWidth, viewHeight, hevc)
        handler.post {
            video = v
            current = this
            open(v)
        }
    }

    private fun open(v: Video) {
        // The system bar, where wanted, takes the bottom of the view from the display.
        bar = SystemBar.height(context, v.viewHeight)
        val displayHeight = v.viewHeight - bar
        touch = touchFor(v, displayHeight)
        val s = ScreenStream(context, handle) { stop() }
        stream = s
        try {
            s.start(v.width, v.height, v.fps, v.viewX, v.viewY, v.viewWidth, displayHeight, bar, v.hevc)
        } catch (e: IOException) {
            Log.e(TAG, "Could not start the screen stream", e)
            stop()
        } catch (e: RuntimeException) {
            Log.e(TAG, "Could not start the screen stream", e)
            stop()
        }
    }

    private fun touchFor(v: Video, displayHeight: Int): Touch {
        val display = Variant.touch(context, v.viewX, v.viewY, v.viewWidth, displayHeight)
        if (bar == 0) return display
        return SystemBar.touch(
            display, v.viewX, v.viewY + displayHeight, v.viewWidth, bar,
            SystemBar.Part.BUTTONS in SystemBar.parts(context),
            { Variant.key(context, SystemBar.KEYCODE_BACK) },
            { Variant.goHome(context) },
            { stream?.showBar(it) },
        )
    }

    /**
     * After a change to the mode or the system bar: the stream built again where the bar comes or
     * goes, since it takes its height from the display; else just what's shown swapped or redrawn.
     */
    fun layoutChanged() {
        handler.post {
            val v = video ?: return@post
            if (SystemBar.height(context, v.viewHeight) == bar) {
                touch = touchFor(v, v.viewHeight - bar)
                stream?.redrawBar()
                stream?.reopen()
                return@post
            }
            stream?.stop()
            stream = null
            open(v)
        }
    }

    /** After a change to what the system bar shows, or whether it shows at all. */
    fun systemBarChanged() {
        handler.post {
            val v = video ?: return@post
            if (SystemBar.height(context, v.viewHeight) == bar) {
                touch = touchFor(v, v.viewHeight - bar)
                stream?.redrawBar()
                return@post
            }
            stream?.stop()
            stream = null
            open(v)
        }
    }

    override fun requestKeyframe() {
        stream?.requestKeyframe()
    }

    override fun touch(action: Int, pointer: Int, x: Float, y: Float) {
        touch?.touch(action, pointer, x, y)
    }

    override fun key(code: Int, down: Boolean) {
        Variant.switchgear(context, code, down)
    }

    override fun ended(error: String?) {
        Log.i(TAG, "Session ended" + if (error == null) "" else ": $error")
        status("Session ended" + (if (error == null) "" else " ($error)") + "; waiting for the head unit")
        handler.post {
            if (current === this) current = null
            video = null
            stream?.stop()
            stream = null
            touch = null
            wifi.leave()
            Bluetooth.closeQuietly(socket)
            listener(this)
        }
    }
}
