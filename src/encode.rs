use std::sync::Arc;

use catplay_carplay::video::{AnnexBIter, AvccConfig, NalType};
use eyre::{Result, eyre};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use futures::StreamExt;
use tokio::sync::{mpsc, watch};
use tokio_util::task::AbortOnDropHandle;

use crate::airplay::EncodedFrame;
use crate::wayland_capture::CapturedFrame;

/// Zero-copy `AsRef<[u8]>` view of a captured frame's pixels, so re-pushing the same frame
/// (which the repeater does 30x/sec for static content) is just an `Arc` refcount bump rather
/// than a full memcpy of the frame each tick.
struct FrameData(Arc<CapturedFrame>);

impl AsRef<[u8]> for FrameData {
    fn as_ref(&self) -> &[u8] {
        &self.0.data
    }
}

/// Where the desktop pixels come from.
pub enum VideoSource {
    /// A PipeWire node handed to us by the xdg-desktop-portal RemoteDesktop/ScreenCast flow.
    Pipewire(u32),
    /// Frames captured directly via `ext-image-copy-capture-v1` (see `wayland_capture.rs`),
    /// published as they arrive - latest frame wins if the encoder falls behind.
    Wayland(watch::Receiver<Option<Arc<CapturedFrame>>>),
}

/// Run a GStreamer pipeline that H264-encodes the desktop and hands each encoded access unit
/// to `video_tx`.
///
/// Uses an in-process `appsink` rather than piping `gst-launch` through an fd: the sink
/// delivers one `alignment=au` buffer per callback, so each message on the channel is
/// already exactly one complete access unit - no byte-stream reframing needed.
pub async fn encode_h264_to_channel(
    source: VideoSource,
    width: u32,
    height: u32,
    video_tx: mpsc::Sender<EncodedFrame>,
) -> Result<()> {
    gst::init()?;

    let pipeline = gst::Pipeline::new();

    let mk = |name: &str, label: &str| {
        gst::ElementFactory::make(name)
            .name(label)
            .build()
            .map_err(|_| eyre!("failed to create {label} ({name})"))
    };

    let src_resolution = gst::Caps::builder("video/x-raw")
        .field("width", width as i32)
        .field("height", height as i32)
        .field("framerate", gst::Fraction::new(30, 1))
        .build();

    // Only populated for VideoSource::Wayland - used below to spawn the task that feeds
    // captured frames into the pipeline.
    let mut wayland_feed: Option<(gst_app::AppSrc, watch::Receiver<Option<Arc<CapturedFrame>>>)> =
        None;

    let src_tail: gst::Element = match source {
        VideoSource::Pipewire(node_id) => {
            let src = mk("pipewiresrc", "source")?;
            src.set_property_from_str("path", &node_id.to_string());

            let scale = mk("videoscale", "videoscale")?;
            let rate = mk("videorate", "videorate")?;

            pipeline.add(&src).map_err(|_| eyre!("add source"))?;
            pipeline.add(&scale).map_err(|_| eyre!("add scale"))?;
            pipeline.add(&rate).map_err(|_| eyre!("add rate"))?;
            src.link(&scale).map_err(|_| eyre!("link src-scale"))?;
            scale.link(&rate).map_err(|_| eyre!("link scale-rate"))?;
            rate.upcast()
        }
        VideoSource::Wayland(frame_rx) => {
            let src = gst::ElementFactory::make("appsrc")
                .name("source")
                .build()
                .map_err(|_| eyre!("failed to create source (appsrc)"))?
                .downcast::<gst_app::AppSrc>()
                .map_err(|_| eyre!("'source' is not an appsrc"))?;
            src.set_property("is-live", true);
            src.set_property_from_str("format", "time");
            // We push buffers with no PTS. Without this, videorate (which needs timestamps)
            // stalls and no frame ever reaches the encoder - the working pipewiresrc path
            // gets proper PTS from the pipewire clock, which is why it "just worked".
            // do-timestamp stamps each buffer with the pipeline running-time as it arrives,
            // exactly like a live capture source.
            src.set_property("do-timestamp", true);

            let scale = mk("videoscale", "videoscale")?;
            let rate = mk("videorate", "videorate")?;

            pipeline.add(&src).map_err(|_| eyre!("add source"))?;
            pipeline.add(&scale).map_err(|_| eyre!("add scale"))?;
            pipeline.add(&rate).map_err(|_| eyre!("add rate"))?;
            src.link(&scale).map_err(|_| eyre!("link src-scale"))?;
            scale.link(&rate).map_err(|_| eyre!("link scale-rate"))?;

            wayland_feed = Some((src.clone(), frame_rx));
            rate.upcast()
        }
    };

    let output_caps = gst::Caps::builder("video/x-h264")
        .field("stream-format", "byte-stream")
        .field("alignment", "au")
        .build();

    // gop-size caps the keyframe interval. After RECORD we drop buffered frames and open
    // the stream on the next keyframe, so this bounds startup latency - the car tears the
    // session down if it gets no video for ~1s. gop-size=6 => a keyframe every ~200ms.
    let enc_desc = std::env::var("CARPLAY_ENCODER")
        .unwrap_or_else(|_| String::from("nvh264enc gop-size=6"));
    let (enc_name, enc_props) = parse_encoder_string(&enc_desc)?;
    let encoder = gst::ElementFactory::make(&enc_name)
        .name("encoder")
        .build()
        .map_err(|_| eyre!("failed to create encoder '{enc_name}'"))?;
    for (key, val) in &enc_props {
        encoder.set_property_from_str(key, val);
    }

    let videoconvert = mk("videoconvert", "videoconvert")?;
    let h264parse = mk("h264parse", "h264parse")?;
    h264parse.set_property("config-interval", -1i32);

    let appsink = mk("appsink", "sink")?;
    appsink.set_property("sync", false);

    pipeline.add(&videoconvert).map_err(|_| eyre!("add vconvert"))?;
    pipeline.add(&encoder).map_err(|_| eyre!("add encoder"))?;
    pipeline.add(&h264parse).map_err(|_| eyre!("add h264parse"))?;
    pipeline.add(&appsink).map_err(|_| eyre!("add appsink"))?;

    src_tail
        .link_filtered(&videoconvert, &src_resolution)
        .map_err(|_| eyre!("link src-vconvert"))?;
    videoconvert
        .link(&encoder)
        .map_err(|_| eyre!("link vconvert-encoder"))?;
    encoder
        .link(&h264parse)
        .map_err(|_| eyre!("link encoder-parse"))?;
    h264parse
        .link_filtered(&appsink, &output_caps)
        .map_err(|_| eyre!("link parse-sink"))?;

    // ---- appsink callbacks ----
    let tx = video_tx.clone();
    let appsink = appsink
        .downcast::<gst_app::AppSink>()
        .map_err(|_| eyre!("'sink' is not an appsink"))?;
    appsink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                let au = normalize_annexb_4byte(map.as_slice());
                let config = extract_avcc_config(&au);
                match tx.blocking_send(EncodedFrame { data: au, config }) {
                    Ok(()) => Ok(gst::FlowSuccess::Ok),
                    Err(_) => Err(gst::FlowError::Eos),
                }
            })
            .build(),
    );

    // Ensure the pipeline is torn down if this future is dropped.
    struct StopGuard(gst::Pipeline);
    impl Drop for StopGuard {
        fn drop(&mut self) {
            let _ = self.0.set_state(gst::State::Null);
        }
    }

    pipeline.set_state(gst::State::Playing)?;
    let _guard = StopGuard(pipeline.clone());

    // Feed the encoder at a steady 30fps by re-pushing the latest captured frame on a timer,
    // rather than only when a new frame arrives. This is essential: the compositor only
    // produces a new capture frame when the desktop actually changes (damage), so a static
    // screen yields no new frames - but the car tears the session down if it gets no video
    // for ~1s. Re-pushing the latest frame keeps a continuous stream (mostly tiny P-frames
    // when static); a real new frame just replaces what we re-push. The `watch` channel is
    // latest-wins, so if capture outpaces us we simply skip older frames.
    let _frame_feeder = wayland_feed.map(|(appsrc, frame_rx)| {
        AbortOnDropHandle::new(tokio::spawn(async move {
            let mut current_caps: Option<(u32, u32, &'static str)> = None;
            let mut ticker = tokio::time::interval(std::time::Duration::from_micros(1_000_000 / 30));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                let Some(frame) = frame_rx.borrow().clone() else {
                    continue;
                };
                let wanted_caps = (frame.width, frame.height, frame.gst_format);
                if current_caps != Some(wanted_caps) {
                    let caps = gst::Caps::builder("video/x-raw")
                        .field("format", frame.gst_format)
                        .field("width", frame.width as i32)
                        .field("height", frame.height as i32)
                        .field("framerate", gst::Fraction::new(30, 1))
                        .build();
                    appsrc.set_caps(Some(&caps));
                    current_caps = Some(wanted_caps);
                }
                let buffer = gst::Buffer::from_slice(FrameData(frame));
                if appsrc.push_buffer(buffer).is_err() {
                    break;
                }
            }
        }))
    });

    let bus = pipeline
        .bus()
        .ok_or_else(|| eyre!("pipeline has no bus"))?;
    let mut messages = bus.stream();
    while let Some(msg) = messages.next().await {
        match msg.view() {
            gst::MessageView::Eos(_) => {
                tracing::info!("GStreamer pipeline reached end of stream");
                break;
            }
            gst::MessageView::Error(err) => {
                return Err(eyre!(
                    "GStreamer error from {:?}: {}",
                    err.src().map(|s| s.path_string()),
                    err.error()
                ));
            }
            _ => {}
        }
    }

    Ok(())
}

/// Split an encoder description like `"nvh264enc gop-size=6"` into
/// a factory name and property list.
fn parse_encoder_string(s: &str) -> Result<(String, Vec<(String, String)>)> {
    let mut parts = s.split_whitespace();
    let name = parts
        .next()
        .ok_or_else(|| eyre!("empty encoder string"))?
        .to_string();
    let mut props = Vec::new();
    for p in parts {
        if let Some(eq) = p.find('=') {
            props.push((p[..eq].to_string(), p[eq + 1..].to_string()));
        }
    }
    Ok((name, props))
}

/// Extract SPS/PPS parameter sets from a keyframe's Annex-B access unit.
fn extract_avcc_config(data: &[u8]) -> Option<AvccConfig> {
    let mut sps = Vec::new();
    let mut pps = Vec::new();

    for chunk in AnnexBIter::annexb(data) {
        let chunk = chunk.ok()?;
        if chunk.data_size == 0 {
            continue;
        }
        let nal_start = chunk.prefix_start + chunk.prefix_len;
        let nal_type = NalType::from_byte(data[nal_start]);
        match nal_type {
            NalType::Sps => {
                sps.extend_from_slice(&data[chunk.prefix_start..nal_start + chunk.data_size])
            }
            NalType::Pps => {
                pps.extend_from_slice(&data[chunk.prefix_start..nal_start + chunk.data_size])
            }
            _ => {}
        }
    }

    if sps.is_empty() {
        return None;
    }
    let mut sps_pps = sps;
    sps_pps.extend_from_slice(&pps);

    Some(AvccConfig {
        nal_size_len: 4,
        sps_pps,
    })
}

/// Rewrite every NAL in an Annex-B access unit behind a 4-byte (`00 00 00 01`) start code.
/// gstreamer emits a mix of 3- and 4-byte prefixes; the AirPlay screen sink only accepts
/// 4-byte ones.
fn normalize_annexb_4byte(buf: &[u8]) -> Vec<u8> {
    let mut cores: Vec<usize> = Vec::new();
    let mut i = 0;
    while i + 3 <= buf.len() {
        if buf[i] == 0 && buf[i + 1] == 0 && buf[i + 2] == 1 {
            cores.push(i);
            i += 3;
        } else {
            i += 1;
        }
    }
    if cores.is_empty() {
        return buf.to_vec();
    }

    let true_start = |p: usize| -> usize {
        let mut s = p;
        while s > 0 && buf[s - 1] == 0 {
            s -= 1;
        }
        s
    };

    let mut out = Vec::with_capacity(buf.len() + cores.len());
    for k in 0..cores.len() {
        let payload_start = cores[k] + 3;
        let payload_end = if k + 1 < cores.len() {
            true_start(cores[k + 1])
        } else {
            buf.len()
        };
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&buf[payload_start..payload_end]);
    }
    out
}
