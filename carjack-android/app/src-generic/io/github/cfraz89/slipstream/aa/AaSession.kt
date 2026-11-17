// Adapted from OpenCfMoto and headunit-revived (AGPL-3.0): aap/AapTransport.kt, aap/AapControl.kt,
// aap/AapMessageHandlerType.kt, protocol/messages/ServiceDiscoveryResponse.kt, aa/AaInput.kt.
package io.github.cfraz89.slipstream.aa

import android.os.SystemClock
import android.util.SparseIntArray
import io.github.cfraz89.slipstream.aa.proto.Common
import io.github.cfraz89.slipstream.aa.proto.Control
import io.github.cfraz89.slipstream.aa.proto.Input
import io.github.cfraz89.slipstream.aa.proto.Media
import io.github.cfraz89.slipstream.aa.proto.Sensors
import io.github.cfraz89.slipstream.aa.proto.Control.Service.MediaSinkService.VideoConfiguration

/**
 * What the head unit asks Android Auto for: a coded frame of [frameWidth]x[frameHeight] whose
 * centre, short of the margins, is the head unit's picture.
 */
class AaGeometry(
    val resolution: VideoConfiguration.VideoCodecResolutionType,
    val frameWidth: Int,
    val frameHeight: Int,
    val marginWidth: Int,
    val marginHeight: Int,
    val density: Int,
)

/**
 * One Android Auto connection, with Slipstream as the head unit: answers its control messages,
 * offers video, touch, keys, sensors and the audio and microphone it won't start without, and
 * hands the video to [decoder]. Audio isn't played: it stays on the phone.
 */
class AaSession(
    private val link: AapLink,
    private val decoder: VideoDecoder,
    private val geometry: AaGeometry,
    private val onEnded: () -> Unit,
) {
    companion object {
        /** Android keycodes, which Android Auto takes as they are. */
        val KEYCODES = intArrayOf(3, 4, 19, 20, 21, 22, 23, 84, 85, 87, 88, 126, 127)
        private const val SENSOR_NIGHT = 10
    }

    private val sessionIds = SparseIntArray()
    @Volatile private var running = false
    /** Down pointers in the order they went down, with their last position. */
    private val pointers = LinkedHashMap<Int, Pair<Int, Int>>()

    fun start() {
        running = true
        decoder.onNeedKeyframe = ::refocus
        Thread({
            if (link.handshake()) {
                while (running) {
                    val message = link.read() ?: break
                    try {
                        handle(message)
                    } catch (e: Exception) {
                        AaLog.e("Couldn't handle a message on channel ${message.channel}", e)
                    }
                }
            }
            end()
        }, "carjack-aa").start()
    }

    fun stop() {
        if (!running) return
        link.sendEncrypted(byebye())
        SystemClock.sleep(150)
        end()
    }

    @Synchronized
    private fun end() {
        if (!running) return
        running = false
        link.close()
        decoder.stop()
        AaLog.i("Session ended")
        onEnded()
    }

    private fun send(message: AapMessage) {
        if (running) link.sendEncrypted(message)
    }

    /** Video focus dropped and taken back, which Android Auto answers with a keyframe. */
    private fun refocus() {
        send(videoFocus(gain = false, unsolicited = false))
        Thread {
            SystemClock.sleep(100)
            send(videoFocus(gain = true, unsolicited = true))
        }.start()
    }

    private fun handle(message: AapMessage) {
        val type = message.type
        if (message.channel == Channel.VIDEO && decoder.process(message)) {
            if (type == Media.MsgType.MEDIA_MESSAGE_DATA_VALUE || type == Media.MsgType.MEDIA_MESSAGE_CODEC_CONFIG_VALUE) {
                send(mediaAck(message.channel, sessionIds.get(message.channel)))
            }
            return
        }
        // Acked so its window never fills, and dropped.
        if (Channel.isAudio(message.channel) && (type == 0 || type == 1)) {
            send(mediaAck(message.channel, sessionIds.get(message.channel)))
            return
        }
        if (type == Control.ControlMsgType.MESSAGE_CHANNEL_OPEN_REQUEST_VALUE) {
            send(
                AapMessage(
                    message.channel, Control.ControlMsgType.MESSAGE_CHANNEL_OPEN_RESPONSE_VALUE,
                    Control.ChannelOpenResponse.newBuilder().setStatus(Common.MessageStatus.STATUS_SUCCESS).build(),
                ),
            )
            if (message.channel == Channel.SENSOR) {
                send(drivingStatus(Sensors.SensorBatch.DrivingStatusData.Status.UNRESTRICTED))
            }
            return
        }
        when (message.channel) {
            Channel.CONTROL -> control(message)
            Channel.SENSOR -> sensor(message)
            Channel.INPUT -> if (type == Input.MsgType.BINDINGREQUEST_VALUE) {
                send(
                    AapMessage(
                        message.channel, Input.MsgType.BINDINGRESPONSE_VALUE,
                        Input.BindingResponse.newBuilder().setStatus(Common.MessageStatus.STATUS_SUCCESS).build(),
                    ),
                )
            }
            else -> if (message.channel == Channel.VIDEO || message.channel == Channel.MIC || Channel.isAudio(message.channel)) {
                media(message)
            }
        }
    }

    private fun control(message: AapMessage) {
        when (message.type) {
            Control.ControlMsgType.MESSAGE_SERVICE_DISCOVERY_REQUEST_VALUE -> {
                val request = message.parse(Control.ServiceDiscoveryRequest.newBuilder()).build()
                AaLog.i("Service discovery from ${request.phoneName}")
                send(discovery())
            }
            Control.ControlMsgType.MESSAGE_PING_REQUEST_VALUE -> send(
                AapMessage(
                    message.channel, Control.ControlMsgType.MESSAGE_PING_RESPONSE_VALUE,
                    Control.PingResponse.newBuilder().setTimestamp(System.nanoTime()).build(),
                ),
            )
            Control.ControlMsgType.MESSAGE_NAV_FOCUS_REQUEST_VALUE -> send(
                AapMessage(
                    message.channel, Control.ControlMsgType.MESSAGE_NAV_FOCUS_NOTIFICATION_VALUE,
                    Control.NavFocusNotification.newBuilder().setFocusType(Control.NavFocusType.NAV_FOCUS_PROJECTED).build(),
                ),
            )
            Control.ControlMsgType.MESSAGE_BYEBYE_REQUEST_VALUE -> {
                AaLog.i("Android Auto said goodbye")
                send(
                    AapMessage(
                        message.channel, Control.ControlMsgType.MESSAGE_BYEBYE_RESPONSE_VALUE,
                        Control.ByeByeResponse.newBuilder().build(),
                    ),
                )
                end()
            }
            Control.ControlMsgType.MESSAGE_BYEBYE_RESPONSE_VALUE -> end()
            Control.ControlMsgType.MESSAGE_AUDIO_FOCUS_REQUEST_VALUE -> {
                // Always granted: the phone plays it, but must believe the head unit has focus.
                val request = message.parse(Control.AudioFocusRequestNotification.newBuilder()).build()
                val state = when (request.request) {
                    Control.AudioFocusRequestNotification.AudioFocusRequestType.GAIN ->
                        Control.AudioFocusNotification.AudioFocusStateType.STATE_GAIN
                    Control.AudioFocusRequestNotification.AudioFocusRequestType.GAIN_TRANSIENT ->
                        Control.AudioFocusNotification.AudioFocusStateType.STATE_GAIN_TRANSIENT
                    Control.AudioFocusRequestNotification.AudioFocusRequestType.GAIN_TRANSIENT_MAY_DUCK ->
                        Control.AudioFocusNotification.AudioFocusStateType.STATE_GAIN_TRANSIENT_GUIDANCE_ONLY
                    else -> Control.AudioFocusNotification.AudioFocusStateType.STATE_LOSS
                }
                send(
                    AapMessage(
                        message.channel, Control.ControlMsgType.MESSAGE_AUDIO_FOCUS_NOTIFICATION_VALUE,
                        Control.AudioFocusNotification.newBuilder().setFocusState(state).build(),
                    ),
                )
            }
        }
    }

    private fun sensor(message: AapMessage) {
        if (message.type != Sensors.SensorsMsgType.SENSOR_STARTREQUEST_VALUE) return
        val request = message.parse(Sensors.SensorRequest.newBuilder()).build()
        send(
            AapMessage(
                message.channel, Sensors.SensorsMsgType.SENSOR_STARTRESPONSE_VALUE,
                Sensors.SensorResponse.newBuilder().setStatus(Common.MessageStatus.STATUS_SUCCESS).build(),
            ),
        )
        if (request.type.number == SENSOR_NIGHT) send(nightMode(false))
    }

    private fun media(message: AapMessage) {
        when (message.type) {
            Media.MsgType.MEDIA_MESSAGE_SETUP_VALUE -> {
                send(
                    AapMessage(
                        message.channel, Media.MsgType.MEDIA_MESSAGE_CONFIG_VALUE,
                        Media.Config.newBuilder()
                            .setStatus(Media.Config.ConfigStatus.STATUS_READY)
                            .setMaxUnacked(if (message.channel == Channel.VIDEO) 12 else 16)
                            .addConfigurationIndices(0)
                            .build(),
                    ),
                )
                // No projection screen to wait for: the encoder's surface is always ready.
                if (message.channel == Channel.VIDEO) send(videoFocus(gain = true, unsolicited = true))
            }
            Media.MsgType.MEDIA_MESSAGE_START_VALUE -> {
                val start = message.parse(Media.Start.newBuilder()).build()
                sessionIds.put(message.channel, start.sessionId)
            }
            Media.MsgType.MEDIA_MESSAGE_VIDEO_FOCUS_REQUEST_VALUE -> {
                val request = message.parse(Media.VideoFocusRequestNotification.newBuilder()).build()
                when (request.mode) {
                    // Exit, in Android Auto.
                    Media.VideoFocusMode.VIDEO_FOCUS_NATIVE -> {
                        AaLog.i("Android Auto exited")
                        stop()
                    }
                    Media.VideoFocusMode.VIDEO_FOCUS_PROJECTED -> send(videoFocus(gain = true, unsolicited = false))
                    else -> {}
                }
            }
        }
    }

    private fun discovery(): AapMessage {
        val pcm16k = Media.AudioConfiguration.newBuilder()
            .setSampleRate(16000).setNumberOfBits(16).setNumberOfChannels(1).build()
        val services = listOf(
            Control.Service.newBuilder().setId(Channel.SENSOR).setSensorSourceService(
                Control.Service.SensorSourceService.newBuilder()
                    .addSensors(Control.Service.SensorSourceService.Sensor.newBuilder().setType(Sensors.SensorType.DRIVING_STATUS))
                    .addSensors(Control.Service.SensorSourceService.Sensor.newBuilder().setType(Sensors.SensorType.NIGHT)),
            ).build(),
            Control.Service.newBuilder().setId(Channel.VIDEO).setMediaSinkService(
                Control.Service.MediaSinkService.newBuilder()
                    .setAvailableType(Media.MediaCodecType.MEDIA_CODEC_VIDEO_H264_BP)
                    .setAudioType(Media.AudioStreamType.NONE)
                    .setAvailableWhileInCall(true)
                    .addVideoConfigs(
                        VideoConfiguration.newBuilder()
                            .setCodecResolution(geometry.resolution)
                            .setFrameRate(VideoConfiguration.VideoFrameRateType._30)
                            .setMarginWidth(geometry.marginWidth)
                            .setMarginHeight(geometry.marginHeight)
                            .setDensity(geometry.density)
                            .setVideoCodecType(Media.MediaCodecType.MEDIA_CODEC_VIDEO_H264_BP),
                    ),
            ).build(),
            Control.Service.newBuilder().setId(Channel.INPUT).setInputSourceService(
                Control.Service.InputSourceService.newBuilder()
                    .addAllKeycodesSupported(KEYCODES.toList())
                    .setTouchscreen(
                        Control.Service.InputSourceService.TouchConfig.newBuilder()
                            .setWidth(geometry.frameWidth).setHeight(geometry.frameHeight),
                    ),
            ).build(),
            // Android Auto drops a head unit with no audio sink or microphone.
            Control.Service.newBuilder().setId(Channel.AUDIO2).setMediaSinkService(
                Control.Service.MediaSinkService.newBuilder()
                    .setAvailableType(Media.MediaCodecType.MEDIA_CODEC_AUDIO_PCM)
                    .setAudioType(Media.AudioStreamType.SYSTEM)
                    .addAudioConfigs(pcm16k),
            ).build(),
            Control.Service.newBuilder().setId(Channel.MIC).setMediaSourceService(
                Control.Service.MediaSourceService.newBuilder()
                    .setType(Media.MediaCodecType.MEDIA_CODEC_AUDIO_PCM)
                    .setAudioConfig(pcm16k),
            ).build(),
        )
        val info = Common.HeadUnitInfo.newBuilder()
            .setMake("Slipstream").setModel("Slipstream").setYear("2026").setVehicleId("slipstream")
            .setHeadUnitMake("Slipstream").setHeadUnitModel("Slipstream")
            .setHeadUnitSoftwareBuild("1").setHeadUnitSoftwareVersion("1")
            .setVehicleType(3)
            .build()
        return AapMessage(
            Channel.CONTROL, Control.ControlMsgType.MESSAGE_SERVICE_DISCOVERY_RESPONSE_VALUE,
            Control.ServiceDiscoveryResponse.newBuilder()
                .addAllServices(services)
                .setMake(info.make).setModel(info.model).setYear(info.year).setVehicleId(info.vehicleId)
                .setDriverPosition(Control.DriverPosition.DRIVER_POSITION_LEFT)
                .setHeadUnitMake(info.headUnitMake).setHeadUnitModel(info.headUnitModel)
                .setHeadUnitSoftwareBuild(info.headUnitSoftwareBuild)
                .setHeadUnitSoftwareVersion(info.headUnitSoftwareVersion)
                .setCanPlayNativeMediaDuringVr(false)
                .setHideProjectedClock(false)
                .setDisplayName("Slipstream")
                .setHeadunitInfo(info)
                .build(),
        )
    }

    /** A key press or release, as an Android keycode. */
    fun key(code: Int, down: Boolean) {
        if (code !in KEYCODES) return
        val keys = Input.KeyEvent.newBuilder()
            .addKeys(Input.Key.newBuilder().setKeycode(code).setDown(down).setMetastate(0).setLongpress(false))
        send(input(Input.InputReport.newBuilder().setKeyEvent(keys)))
    }

    /**
     * A finger in the coded frame's pixels, [action] a [io.github.cfraz89.slipstream.Touch] one.
     * Every event carries every finger down, the second and later going down and up as pointer
     * actions, so Maps can pinch.
     */
    @Synchronized
    fun touch(action: Int, pointer: Int, x: Int, y: Int) {
        val pointerAction = when (action) {
            io.github.cfraz89.slipstream.Touch.DOWN, io.github.cfraz89.slipstream.Touch.MOVE -> {
                val isNew = !pointers.containsKey(pointer)
                pointers[pointer] = x to y
                when {
                    !isNew -> Input.TouchEvent.PointerAction.TOUCH_ACTION_MOVE
                    pointers.size == 1 -> Input.TouchEvent.PointerAction.TOUCH_ACTION_DOWN
                    else -> Input.TouchEvent.PointerAction.TOUCH_ACTION_POINTER_DOWN
                }
            }
            io.github.cfraz89.slipstream.Touch.UP -> {
                if (!pointers.containsKey(pointer)) return
                pointers[pointer] = x to y
                if (pointers.size == 1) {
                    Input.TouchEvent.PointerAction.TOUCH_ACTION_UP
                } else {
                    Input.TouchEvent.PointerAction.TOUCH_ACTION_POINTER_UP
                }
            }
            else -> return
        }
        val event = Input.TouchEvent.newBuilder()
            .setAction(pointerAction)
            .setActionIndex(pointers.keys.indexOf(pointer).coerceAtLeast(0))
        for ((id, pos) in pointers) {
            event.addPointerData(Input.TouchEvent.Pointer.newBuilder().setX(pos.first).setY(pos.second).setPointerId(id))
        }
        if (action == io.github.cfraz89.slipstream.Touch.UP) pointers.remove(pointer)
        send(input(Input.InputReport.newBuilder().setTouchEvent(event)))
    }

    /** Stamped with a monotonic microsecond clock, as Android Auto wants. */
    private fun input(report: Input.InputReport.Builder) = AapMessage(
        Channel.INPUT, Input.MsgType.EVENT_VALUE,
        report.setTimestamp(SystemClock.elapsedRealtimeNanos() / 1000).build(),
    )
}
