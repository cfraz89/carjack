// Adapted from OpenCfMoto and headunit-revived (AGPL-3.0): aap/AapMessage.kt, Messages.kt,
// Channel.kt and the small protocol messages.
package io.github.cfraz89.slipstream.aa

import com.google.protobuf.CodedOutputStream
import com.google.protobuf.MessageLite
import io.github.cfraz89.slipstream.aa.proto.Control
import io.github.cfraz89.slipstream.aa.proto.Media
import io.github.cfraz89.slipstream.aa.proto.Sensors

/** Android Auto's channels: one per service the head unit offers in discovery. */
object Channel {
    const val CONTROL = 0
    const val SENSOR = 1
    const val VIDEO = 2
    const val INPUT = 3
    const val AUDIO1 = 4
    const val AUDIO2 = 5
    const val AUDIO = 6
    const val MIC = 7

    fun isAudio(channel: Int) = channel == AUDIO || channel == AUDIO1 || channel == AUDIO2
}

/**
 * One message on the wire: channel, flags, a 16-bit length, then a 16-bit type and the protobuf.
 * Built from a proto to send, or decrypted from what Android Auto sent.
 */
open class AapMessage(
    val channel: Int,
    val flags: Byte,
    val type: Int,
    val dataOffset: Int,
    val size: Int,
    val data: ByteArray,
) {
    constructor(channel: Int, type: Int, proto: MessageLite) :
        this(channel, flags(type, channel), type, HEADER_SIZE + TYPE_SIZE, size(proto), ByteArray(size(proto))) {
        data[0] = channel.toByte()
        data[1] = flags
        putShort(proto.serializedSize + TYPE_SIZE, 2, data)
        putShort(type, 4, data)
        val out = CodedOutputStream.newInstance(data, HEADER_SIZE + TYPE_SIZE, proto.serializedSize)
        proto.writeTo(out)
        out.checkNoSpaceLeft()
    }

    fun <T : MessageLite.Builder> parse(builder: T): T {
        builder.mergeFrom(data, dataOffset, size - dataOffset)
        return builder
    }

    companion object {
        const val HEADER_SIZE = 4
        const val TYPE_SIZE = 2
        /** Big enough for any one message's encrypted body. */
        const val BUFFER_LENGTH = 131080

        private fun size(proto: MessageLite) = proto.serializedSize + TYPE_SIZE + HEADER_SIZE

        /** A control-type message on a channel other than control is flagged as such. */
        private fun flags(type: Int, channel: Int): Byte =
            if (channel != Channel.CONTROL && type in 1..26) 0x0f else 0x0b

        fun putShort(value: Int, offset: Int, buf: ByteArray) {
            buf[offset] = (value shr 8).toByte()
            buf[offset + 1] = value.toByte()
        }

        fun getShort(buf: ByteArray, offset: Int) =
            ((buf[offset].toInt() and 0xFF) shl 8) or (buf[offset + 1].toInt() and 0xFF)

        /** A plaintext message, as the version exchange and the TLS handshake go. */
        fun raw(channel: Int, flags: Int, type: Int, payload: ByteArray): ByteArray {
            val buf = ByteArray(6 + payload.size)
            buf[0] = channel.toByte()
            buf[1] = flags.toByte()
            putShort(payload.size + TYPE_SIZE, 2, buf)
            putShort(type, 4, buf)
            System.arraycopy(payload, 0, buf, 6, payload.size)
            return buf
        }

        /** Protocol 1.2. */
        val VERSION_REQUEST = raw(0, 3, 1, byteArrayOf(0, 1, 0, 2))
        /** Authentication done, status OK. */
        val AUTH_COMPLETE = raw(0, 3, 4, byteArrayOf(8, 0))
    }
}

fun mediaAck(channel: Int, sessionId: Int) = AapMessage(
    channel, Media.MsgType.MEDIA_MESSAGE_ACK_VALUE,
    Media.Ack.newBuilder().setSessionId(sessionId).setAck(1).build(),
)

fun videoFocus(gain: Boolean, unsolicited: Boolean) = AapMessage(
    Channel.VIDEO, Media.MsgType.MEDIA_MESSAGE_VIDEO_FOCUS_NOTIFICATION_VALUE,
    Media.VideoFocusNotification.newBuilder()
        .setMode(if (gain) Media.VideoFocusMode.VIDEO_FOCUS_PROJECTED else Media.VideoFocusMode.VIDEO_FOCUS_NATIVE)
        .setUnsolicited(unsolicited)
        .build(),
)

fun drivingStatus(status: Sensors.SensorBatch.DrivingStatusData.Status) = AapMessage(
    Channel.SENSOR, Sensors.SensorsMsgType.SENSOR_EVENT_VALUE,
    Sensors.SensorBatch.newBuilder()
        .addDrivingStatus(Sensors.SensorBatch.DrivingStatusData.newBuilder().setStatus(status.number))
        .build(),
)

fun nightMode(night: Boolean) = AapMessage(
    Channel.SENSOR, Sensors.SensorsMsgType.SENSOR_EVENT_VALUE,
    Sensors.SensorBatch.newBuilder()
        .addNightMode(Sensors.SensorBatch.NightData.newBuilder().setIsNightMode(night))
        .build(),
)

fun byebye() = AapMessage(
    Channel.CONTROL, Control.ControlMsgType.MESSAGE_BYEBYE_REQUEST_VALUE,
    Control.ByeByeRequest.newBuilder().setReason(Control.ByeByeReason.USER_SELECTION).build(),
)
