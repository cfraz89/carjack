use std::os::fd::AsFd;
use std::sync::Arc;

use catplay_carplay::video::{AnnexBIter, AvccConfig, NalType};
use eyre::{Result, eyre};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_allocators::prelude::*;
use gstreamer_allocators::DmaBufAllocator;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use futures::StreamExt;
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::Buffer as _;
use tokio::sync::{mpsc, watch};
use tokio_util::task::AbortOnDropHandle;

use crate::airplay::EncodedFrame;
use crate::compositor::CapturedFrame;

/// Zero-copy `AsRef<[u8]>` view of an SHM frame's pixels, so re-pushing the same frame (the
/// repeater does 30x/sec) is just an `Arc` refcount bump rather than a full memcpy each tick.
struct FrameData(Arc<CapturedFrame>);

impl AsRef<[u8]> for FrameData {
    fn as_ref(&self) -> &[u8] {
        match &*self.0 {
            CapturedFrame::Shm { data, .. } => data,
            CapturedFrame::Dmabuf(_) => &[],
        }
    }
}

/// Run a GStreamer pipeline that H264-encodes the composited desktop frames (from the internal
/// compositor, via `frame_rx`) and hands each encoded access unit to `video_tx`.
///
/// Uses an in-process `appsink` rather than piping `gst-launch` through an fd: the sink
/// delivers one `alignment=au` buffer per callback, so each message on the channel is
/// already exactly one complete access unit - no byte-stream reframing needed.
pub async fn encode_h264_to_channel(
    frame_rx: watch::Receiver<Option<Arc<CapturedFrame>>>,
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

    // appsrc fed by the compositor's frame stream. do-timestamp stamps each pushed buffer with
    // the pipeline running-time (buffers arrive with no PTS, and videorate needs timestamps).
    let src = gst::ElementFactory::make("appsrc")
        .name("source")
        .build()
        .map_err(|_| eyre!("failed to create source (appsrc)"))?
        .downcast::<gst_app::AppSrc>()
        .map_err(|_| eyre!("'source' is not an appsrc"))?;
    src.set_property("is-live", true);
    src.set_property_from_str("format", "time");
    src.set_property("do-timestamp", true);
    pipeline.add(&src).map_err(|_| eyre!("add source"))?;

    // Zero-copy: the compositor forwards its rendered dmabuf and we import it straight into the
    // GPU encoder (glupload -> nvh264enc), no CPU copy. Otherwise the software chain encodes the
    // CPU-read-back frames. The compositor and encoder read the same CARJACK_ZEROCOPY flag so
    // the frame variant (Dmabuf vs Shm) always matches the pipeline.
    let zero_copy = std::env::var("CARJACK_ZEROCOPY").is_ok();
    let src_tail: gst::Element = src.clone().upcast();
    let encoder = if zero_copy {
        build_zerocopy_chain(&pipeline, &src_tail)?
    } else {
        build_encode_chain(&pipeline, &src_tail, width, height)?
    };
    let wayland_feed = Some((src, frame_rx));

    let output_caps = gst::Caps::builder("video/x-h264")
        .field("stream-format", "byte-stream")
        .field("alignment", "au")
        .build();

    let h264parse = mk("h264parse", "h264parse")?;
    h264parse.set_property("config-interval", -1i32);

    let appsink = mk("appsink", "sink")?;
    appsink.set_property("sync", false);

    pipeline.add(&h264parse).map_err(|_| eyre!("add h264parse"))?;
    pipeline.add(&appsink).map_err(|_| eyre!("add appsink"))?;

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
            let dmabuf_allocator = DmaBufAllocator::new();
            let mut caps_set = false;
            let mut ticker = tokio::time::interval(std::time::Duration::from_micros(1_000_000 / 30));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                let Some(frame) = frame_rx.borrow().clone() else {
                    continue;
                };
                let buffer = match &*frame {
                    CapturedFrame::Shm { width, height, gst_format, .. } => {
                        if !caps_set {
                            appsrc.set_caps(Some(
                                &gst::Caps::builder("video/x-raw")
                                    .field("format", *gst_format)
                                    .field("width", *width as i32)
                                    .field("height", *height as i32)
                                    .field("framerate", gst::Fraction::new(30, 1))
                                    .build(),
                            ));
                            caps_set = true;
                        }
                        gst::Buffer::from_slice(FrameData(frame.clone()))
                    }
                    CapturedFrame::Dmabuf(dmabuf) => {
                        if !caps_set {
                            match dmabuf_caps(dmabuf) {
                                Ok(caps) => appsrc.set_caps(Some(&caps)),
                                Err(e) => {
                                    tracing::warn!("dmabuf caps failed: {e}");
                                    continue;
                                }
                            }
                            caps_set = true;
                        }
                        match wrap_dmabuf(&dmabuf_allocator, dmabuf) {
                            Ok(buf) => buf,
                            Err(e) => {
                                tracing::warn!("wrap dmabuf failed: {e}");
                                continue;
                            }
                        }
                    }
                };
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

/// Zero-copy encode chain: `glupload` imports the compositor's dmabuf as a GL texture (via
/// EGLImage, zero-copy) and `nvh264enc` consumes it through GL-CUDA interop (also zero-copy),
/// converting RGB->NV12 internally. No scale/rate/convert - the compositor already renders at
/// the car's resolution and 30fps. Requires the compositor to render on the same GPU as the
/// encoder (CARJACK_RENDER_NODE) so the dmabuf is GPU-local.
fn build_zerocopy_chain(pipeline: &gst::Pipeline, src_tail: &gst::Element) -> Result<gst::Element> {
    let glupload = gst::ElementFactory::make("glupload")
        .name("glupload")
        .build()
        .map_err(|_| eyre!("failed to create glupload"))?;
    let enc_desc =
        std::env::var("CARPLAY_ENCODER").unwrap_or_else(|_| String::from("nvh264enc gop-size=6"));
    let (enc_name, enc_props) = parse_encoder_string(&enc_desc)?;
    let encoder = gst::ElementFactory::make(&enc_name)
        .name("encoder")
        .build()
        .map_err(|_| eyre!("failed to create encoder '{enc_name}'"))?;
    for (key, val) in &enc_props {
        encoder.set_property_from_str(key, val);
    }
    pipeline.add(&glupload).map_err(|_| eyre!("add glupload"))?;
    pipeline.add(&encoder).map_err(|_| eyre!("add encoder"))?;
    src_tail.link(&glupload).map_err(|_| eyre!("link src-glupload"))?;
    glupload.link(&encoder).map_err(|_| eyre!("link glupload-encoder"))?;
    tracing::info!("Encode backend: zero-copy dmabuf (glupload ! {enc_name})");
    Ok(encoder)
}

/// DMA_DRM caps for a dmabuf - carries the DRM fourcc + modifier so glupload can import it.
fn dmabuf_caps(dmabuf: &Dmabuf) -> Result<gst::Caps> {
    let info = gst_video::VideoInfo::builder(
        gst_video::VideoFormat::DmaDrm,
        dmabuf.width(),
        dmabuf.height(),
    )
    .fps(gst::Fraction::new(30, 1))
    .build()
    .map_err(|e| eyre!("build video info: {e}"))?;
    let fourcc = dmabuf.format().code as u32;
    let modifier: u64 = dmabuf.format().modifier.into();
    gst_video::VideoInfoDmaDrm::new(info, fourcc, modifier)
        .to_caps()
        .map_err(|e| eyre!("build dma-drm caps: {e}"))
}

/// Wrap a dmabuf's plane fds as a dmabuf-backed `gst::Buffer` with a `VideoMeta`, so glupload
/// can import it. Fds are dup'd; GStreamer owns its copies.
fn wrap_dmabuf(allocator: &DmaBufAllocator, dmabuf: &Dmabuf) -> Result<gst::Buffer> {
    let (width, height) = (dmabuf.width(), dmabuf.height());
    let mut buffer = gst::Buffer::new();
    let bm = buffer.get_mut().ok_or_else(|| eyre!("buffer not uniquely owned"))?;
    let mut offsets = Vec::new();
    let mut strides = Vec::new();
    for (fd, (offset, stride)) in dmabuf.handles().zip(dmabuf.offsets().zip(dmabuf.strides())) {
        let owned = fd.as_fd().try_clone_to_owned().map_err(|e| eyre!("dup dmabuf fd: {e}"))?;
        let size = (stride as usize) * (height as usize) + offset as usize;
        let mem = unsafe {
            allocator
                .alloc_dmabuf(owned, size)
                .map_err(|e| eyre!("alloc dmabuf memory: {e}"))?
        };
        bm.append_memory(mem);
        offsets.push(offset as usize);
        strides.push(stride as i32);
    }
    gst_video::VideoMeta::add_full(
        bm,
        gst_video::VideoFrameFlags::empty(),
        gst_video::VideoFormat::DmaDrm,
        width,
        height,
        &offsets,
        &strides,
    )
    .map_err(|e| eyre!("attach video meta: {e}"))?;
    Ok(buffer)
}

/// A hardware zero-copy encode backend: a dmabuf-capable post-processor (does RGB->NV12 +
/// resize to the car's resolution on the GPU) feeding a hardware H264 encoder. Selected only
/// when *both* its elements are available (probed at runtime via `ElementFactory::find`), so a
/// machine without the driver silently falls back to software - the same auto-negotiation the
/// working `pipewiresrc` path got for free.
///
/// Adding a platform = one row here. Keep the highest-value/most-specific backends first.
struct HwBackend {
    /// Value for `CARPLAY_ENCODE_BACKEND` to force this backend, and what gets logged.
    name: &'static str,
    /// dmabuf-importing convert+scale element (M2M): RGB->NV12 + resize, on-GPU, zero-copy.
    postproc: &'static str,
    /// hardware H264 encoder consuming the post-processor's output.
    encoder: &'static str,
    /// (property, value) that caps the keyframe interval to ~6 frames. The car tears the
    /// session down after ~1s without a keyframe, so this bounds startup/recovery latency.
    /// Property name differs per encoder; validate against the real element when enabling.
    keyframe_prop: (&'static str, &'static str),
}

/// Probed in order; first whose `postproc` + `encoder` both exist wins. Entries are validated
/// per-platform as the hardware becomes testable (VA-API on Intel/AMD dev machines, v4l2 on the
/// Qualcomm target).
///
/// NOTE: full zero-copy additionally needs the compositor to forward its rendered dmabuf fd
/// (instead of the CPU readback in compositor.rs) and to render on the *same* GPU as the
/// encoder. Until then these give hardware encode of the already-composited frames, and only
/// auto-select where the elements exist.
const HW_BACKENDS: &[HwBackend] = &[
    // Qualcomm / embedded (the deployment target). Untested off-device; keyframe control on
    // v4l2 stateful encoders is usually via extra-controls, so this is a best guess to revisit.
    HwBackend {
        name: "v4l2",
        postproc: "v4l2convert",
        encoder: "v4l2h264enc",
        keyframe_prop: ("", ""),
    },
    // Intel / AMD VA-API.
    HwBackend {
        name: "vaapi",
        postproc: "vapostproc",
        encoder: "vah264enc",
        keyframe_prop: ("key-int-max", "6"),
    },
];

fn element_available(name: &str) -> bool {
    gst::ElementFactory::find(name).is_some()
}

/// Pick the hardware backend to use: an explicit `CARPLAY_ENCODE_BACKEND` override
/// (`software` forces the fallback), else the first available `HW_BACKENDS` entry, else none
/// (software).
fn select_hw_backend() -> Option<&'static HwBackend> {
    match std::env::var("CARPLAY_ENCODE_BACKEND").ok().as_deref() {
        Some("software") => None,
        Some(forced) => HW_BACKENDS
            .iter()
            .find(|b| b.name == forced)
            .or_else(|| {
                tracing::warn!("CARPLAY_ENCODE_BACKEND={forced} unknown, using software");
                None
            }),
        None => HW_BACKENDS
            .iter()
            .find(|b| element_available(b.postproc) && element_available(b.encoder)),
    }
}

/// Build `src_tail` (raw video) -> scale + RGB->NV12 convert + H264 encode, returning the
/// element whose output is H264. Uses a hardware backend if one is available/forced, otherwise
/// the software chain (videoscale ! videorate ! videoconvert ! `CARPLAY_ENCODER`).
fn build_encode_chain(
    pipeline: &gst::Pipeline,
    src_tail: &gst::Element,
    width: u32,
    height: u32,
) -> Result<gst::Element> {
    let mk = |name: &str, label: &str| {
        gst::ElementFactory::make(name)
            .name(label)
            .build()
            .map_err(|_| eyre!("failed to create {label} ({name})"))
    };
    // Enforces the car's exact display size (head units reject a mismatched stream) and 30fps.
    let target_caps = gst::Caps::builder("video/x-raw")
        .field("width", width as i32)
        .field("height", height as i32)
        .field("framerate", gst::Fraction::new(30, 1))
        .build();

    if let Some(b) = select_hw_backend() {
        tracing::info!("Encode backend: hardware '{}' ({} ! {})", b.name, b.postproc, b.encoder);
        let postproc = mk(b.postproc, "postproc")?;
        let encoder = mk(b.encoder, "encoder")?;
        if !b.keyframe_prop.0.is_empty() {
            encoder.set_property_from_str(b.keyframe_prop.0, b.keyframe_prop.1);
        }
        pipeline.add(&postproc).map_err(|_| eyre!("add postproc"))?;
        pipeline.add(&encoder).map_err(|_| eyre!("add encoder"))?;
        src_tail.link(&postproc).map_err(|_| eyre!("link src-postproc"))?;
        postproc
            .link_filtered(&encoder, &target_caps)
            .map_err(|_| eyre!("link postproc-encoder"))?;
        return Ok(encoder);
    }

    tracing::info!("Encode backend: software (videoscale ! videorate ! videoconvert ! encoder)");
    // gop-size caps the keyframe interval (see keyframe_prop above for why).
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
    let scale = mk("videoscale", "videoscale")?;
    let rate = mk("videorate", "videorate")?;
    let convert = mk("videoconvert", "videoconvert")?;

    pipeline.add(&scale).map_err(|_| eyre!("add scale"))?;
    pipeline.add(&rate).map_err(|_| eyre!("add rate"))?;
    pipeline.add(&convert).map_err(|_| eyre!("add convert"))?;
    pipeline.add(&encoder).map_err(|_| eyre!("add encoder"))?;

    src_tail.link(&scale).map_err(|_| eyre!("link src-scale"))?;
    scale.link(&rate).map_err(|_| eyre!("link scale-rate"))?;
    rate.link(&convert).map_err(|_| eyre!("link rate-convert"))?;
    convert
        .link_filtered(&encoder, &target_caps)
        .map_err(|_| eyre!("link convert-encoder"))?;
    Ok(encoder)
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
