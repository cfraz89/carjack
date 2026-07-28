use std::os::fd::{AsFd, AsRawFd};
use std::sync::Arc;

use catplay_carplay::video::{AnnexBIter, AvccConfig, NalType};
use eyre::{Result, eyre};
use futures::StreamExt;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_allocators::DmaBufAllocator;
use gstreamer_allocators::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use smithay::backend::allocator::Buffer as _;
use smithay::backend::allocator::dmabuf::Dmabuf;
use tokio::sync::{mpsc, watch};
use tokio_util::task::AbortOnDropHandle;

use crate::compositor::CapturedFrame;
use crate::config::ZeroCopyMode;
use libcarjack::airplay::EncodedFrame;

/// The configured target video bitrate (`[graphics] bitrate` / `--bitrate`, in kbit/s), passed to
/// whichever encoder is selected. A newtype because the encoders disagree on units - x264enc,
/// nvh264enc and vah264enc take kbit/s while openh264enc and the v4l2 controls take bit/s - so the
/// encoder descriptions in [`HW_BACKENDS`] / [`DEFAULT_SW_ENCODER`] spell the unit they want as a
/// `{kbps}` / `{bps}` placeholder and [`Bitrate::apply`] fills it in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bitrate {
    kbps: u32,
}

impl Bitrate {
    /// From kbit/s, which is how it's configured.
    pub fn from_kbps(kbps: u32) -> Self {
        Self { kbps }
    }

    /// Substitute this bitrate into a gst-launch-style encoder description, replacing `{kbps}` with
    /// kbit/s and `{bps}` with bit/s. A description with neither placeholder is returned unchanged
    /// (`v4l2h264enc`'s bare default, and any `CARPLAY_ENCODER` override that pins its own rate).
    fn apply(self, desc: &str) -> String {
        desc.replace("{kbps}", &self.kbps.to_string())
            .replace("{bps}", &(self.kbps as u64 * 1000).to_string())
    }
}

impl Default for Bitrate {
    fn default() -> Self {
        Self::from_kbps(crate::config::DEFAULT_BITRATE_KBPS)
    }
}

impl std::fmt::Display for Bitrate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} kbit/s", self.kbps)
    }
}

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

/// How the appsrc feeder turns each `CapturedFrame` into a `gst::Buffer` (and what caps it
/// announces on the appsrc). Chosen together with the encode chain by [`build_backend`], so the
/// buffer kind always matches what the first pipeline element can consume.
enum FrameFeeder {
    /// Software path: system-memory BGRx pushed as plain `video/x-raw`.
    Shm,
    /// VA-API / v4l2 zero-copy: the compositor's dmabuf pushed as `video/x-raw(memory:DMABuf)`
    /// (DMA_DRM), imported directly by the postproc element (`vapostproc` / `v4l2convert`).
    DmaDrm(DmaBufAllocator),
    /// NVIDIA zero-copy: each dmabuf imported into `GstCudaMemory` via CUDA/EGL interop, pushed
    /// as `video/x-raw(memory:CUDAMemory)` straight into `nvh264enc` (which does RGB->NV12). The
    /// importer (its CUDA context) is created once at startup and shared across encode sessions.
    Cuda(Arc<cuda_dmabuf::CudaImporter>),
}

/// The resolved encode backend, decided once at startup ([`resolve_backend`]) and reused for
/// every session's encoder. Cloneable so each session's encode task gets its own handle (the CUDA
/// importer is shared behind an `Arc`).
#[derive(Clone)]
pub enum EncodeBackend {
    /// Zero-copy through a row of [`HW_BACKENDS`]. `importer` is `Some` exactly for backends whose
    /// [`Import`] is `Cuda` - it owns the CUDA context, so it's built once at startup and shared
    /// by every session.
    Hw {
        backend: &'static HwBackend,
        importer: Option<Arc<cuda_dmabuf::CudaImporter>>,
    },
    /// CPU path: the compositor reads frames back to system memory and a software encoder (or, if
    /// the machine has none, a hardware one fed system memory) takes them.
    Software,
}

impl EncodeBackend {
    /// Whether this backend consumes the compositor's dmabuf directly (so the compositor must
    /// forward dmabufs); false means the software path consuming CPU-read-back frames.
    pub fn is_zero_copy(&self) -> bool {
        !matches!(self, Self::Software)
    }

    /// Short human-readable name of the selected backend, for logging.
    pub fn describe(&self) -> String {
        match self {
            Self::Hw { backend, .. } => format!("zero-copy hardware '{}'", backend.name),
            Self::Software => "software".into(),
        }
    }
}

/// Initialise GStreamer up front, off the async runtime.
///
/// The first `gst::init()` builds the plugin registry, which takes seconds; keep that out of a
/// session. The app path gets this free via [`resolve_backend`]; desktop mode has to ask.
/// Blocking and idempotent.
pub fn init_gstreamer() -> Result<()> {
    gst::init()?;
    Ok(())
}

/// Decide the encode backend once, at startup, honouring the configured [`ZeroCopyMode`]:
/// - `Off` -> software.
/// - `Force` -> a zero-copy backend or an error (never silently downgrades).
/// - `Auto` -> a zero-copy backend if one comes up, else software with a warning.
///
/// `render_node` is the GPU the compositor renders on; a zero-copy backend must live on the same
/// GPU (the CUDA importer opens exactly this node).
pub fn resolve_backend(mode: ZeroCopyMode, render_node: &str) -> Result<EncodeBackend> {
    let _ = gst::init();
    match mode {
        ZeroCopyMode::Off => Ok(EncodeBackend::Software),
        ZeroCopyMode::Force => try_zero_copy(render_node).ok_or_else(|| {
            eyre!(
                "zerocopy=true but no zero-copy backend is available on {render_node} (tried: {})",
                HW_BACKENDS
                    .iter()
                    .map(HwBackend::describe_elements)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }),
        ZeroCopyMode::Auto => Ok(try_zero_copy(render_node).unwrap_or_else(|| {
            tracing::warn!(
                "zerocopy=auto: no zero-copy backend available on {render_node}, \
                 falling back to SOFTWARE encode"
            );
            EncodeBackend::Software
        })),
    }
}

/// Try to construct a zero-copy backend for `render_node` by probing [`HW_BACKENDS`] in order,
/// honouring an explicit `CARPLAY_ENCODE_BACKEND` (`cuda`/`vaapi`/`v4l2`) override. Returns `None`
/// if none are usable.
fn try_zero_copy(render_node: &str) -> Option<EncodeBackend> {
    for backend in HW_BACKENDS.iter().filter(|b| wanted(b) && b.available()) {
        match backend.import {
            Import::DmaBuf => {
                return Some(EncodeBackend::Hw {
                    backend,
                    importer: None,
                });
            }
            // CUDA is the one backend that needs state (a CUDA context bound to `render_node`)
            // rather than just elements, so it can fail this late - e.g. the compositor renders on
            // a non-NVIDIA node. Fall through to the next backend if it does.
            Import::Cuda => match cuda_dmabuf::CudaImporter::new(render_node) {
                Ok(imp) => {
                    return Some(EncodeBackend::Hw {
                        backend,
                        importer: Some(Arc::new(imp)),
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        "'{}' zero-copy unavailable on {render_node}: {e}",
                        backend.name
                    )
                }
            },
        }
    }
    None
}

/// Whether `CARPLAY_ENCODE_BACKEND` allows this backend (unset = all of them). Applies to both
/// the internal-compositor and desktop paths.
fn wanted(b: &HwBackend) -> bool {
    std::env::var("CARPLAY_ENCODE_BACKEND")
        .ok()
        .is_none_or(|f| f.eq_ignore_ascii_case(b.name))
}

/// Run a GStreamer pipeline that H264-encodes the composited desktop frames (from the internal
/// compositor, via `frame_rx`) and hands each encoded access unit to `video_tx`.
///
/// Uses an in-process `appsink` rather than piping `gst-launch` through an fd: the sink
/// delivers one `alignment=au` buffer per callback, so each message on the channel is
/// already exactly one complete access unit - no byte-stream reframing needed.
pub async fn encode_h264_to_channel(
    frame_rx: watch::Receiver<Option<Arc<CapturedFrame>>>,
    video_tx: mpsc::Sender<EncodedFrame>,
    backend: EncodeBackend,
    bitrate: Bitrate,
    keyframe_rx: mpsc::Receiver<()>,
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
    // the pipeline running-time (buffers arrive with no PTS, and the encoder needs timestamps).
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

    // A small leaky queue decouples the feeder from the encode chain and, crucially, keeps
    // latency bounded: if the encoder or the car's WiFi momentarily falls behind, it drops the
    // oldest frame (leaky=downstream) rather than letting buffers pile up and lag grow. It's a
    // live stream, so a dropped frame is always better than a growing delay.
    let queue = mk("queue", "queue")?;
    queue.set_property_from_str("leaky", "downstream");
    queue.set_property("max-size-buffers", 3u32);
    queue.set_property("max-size-time", 0u64);
    queue.set_property("max-size-bytes", 0u32);
    pipeline.add(&queue).map_err(|_| eyre!("add queue"))?;
    src.link(&queue).map_err(|_| eyre!("link src-queue"))?;

    // Build the pipeline for the backend chosen at startup. The compositor was started in a
    // matching mode (dmabuf for zero-copy, CPU-readback SHM for software), so the frame variant
    // the feeder receives always matches what this pipeline's first element can consume.
    let (encoder, feeder) = build_backend(&pipeline, &queue, &backend, bitrate)?;
    tracing::info!("Encode backend: {} at {bitrate}", backend.describe());

    let _frame_feeder = AbortOnDropHandle::new(tokio::spawn(async move {
        let appsrc = src;
        let mut frame_rx = frame_rx;
        let mut caps_set = false;
        let mut warned_mismatch = false;
        loop {
            if frame_rx.changed().await.is_err() {
                break; // compositor gone
            }
            let Some(frame) = frame_rx.borrow_and_update().clone() else {
                continue;
            };
            let buffer = match (&feeder, &*frame) {
                (
                    FrameFeeder::Shm,
                    CapturedFrame::Shm {
                        width,
                        height,
                        gst_format,
                        ..
                    },
                ) => {
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
                (FrameFeeder::DmaDrm(allocator), CapturedFrame::Dmabuf(dmabuf)) => {
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
                    match wrap_dmabuf(allocator, dmabuf) {
                        Ok(buf) => buf,
                        Err(e) => {
                            tracing::warn!("wrap dmabuf failed: {e}");
                            continue;
                        }
                    }
                }
                (FrameFeeder::Cuda(importer), CapturedFrame::Dmabuf(dmabuf)) => {
                    if !caps_set {
                        appsrc.set_caps(Some(&cuda_caps(dmabuf)));
                        caps_set = true;
                    }
                    let Some(plane) = first_plane(dmabuf) else {
                        tracing::warn!("dmabuf has no plane 0");
                        continue;
                    };
                    match importer.import(&plane) {
                        Ok(buf) => buf,
                        Err(e) => {
                            tracing::warn!("cuda import failed: {e}");
                            continue;
                        }
                    }
                }
                _ => {
                    if !warned_mismatch {
                        tracing::warn!("frame variant does not match the selected encode backend");
                        warned_mismatch = true;
                    }
                    continue;
                }
            };
            if appsrc.push_buffer(buffer).is_err() {
                break;
            }
        }
    }));

    // `_frame_feeder` stays alive for the whole call, feeding the appsrc while the pipeline runs.
    drive_h264_pipeline(&pipeline, &encoder, video_tx, keyframe_rx).await
}

/// Stream the user's existing desktop, captured via the xdg-desktop-portal ScreenCast PipeWire
/// `node_id`, scaled/re-timed to the car's `width`x`height` @ 30fps. Used for
/// `source = "desktop"`; input is injected back through the portal (see `src/portal.rs`), not
/// here. `pipewiresrc` is a live source with its own timestamps, so no appsrc/feeder is needed.
///
/// `zerocopy` selects the convert/scale path:
/// - **GPU zero-copy** (`Auto`/`Force`, when a [`HW_BACKENDS`] row's `desktop` elements exist):
///   `pipewiresrc ! <backend desktop chain> ! videorate ! <backend encoder>` - the chain imports
///   the portal's dmabuf and does convert/scale/retime on the GPU (`videorate` is
///   memory-agnostic). The backend on the GPU that renders the desktop is preferred.
/// - **Software** (`Off`, or `Auto` with no usable backend): `pipewiresrc ! videorate !
///   videoscale ! videoconvert ! <DEFAULT_SW_ENCODER>` - convert/scale on the CPU.
pub async fn encode_pipewire_to_channel(
    node_id: u32,
    width: u32,
    height: u32,
    zerocopy: ZeroCopyMode,
    bitrate: Bitrate,
    video_tx: mpsc::Sender<EncodedFrame>,
    desktop_size_tx: tokio::sync::watch::Sender<Option<(u32, u32)>>,
    keyframe_rx: mpsc::Receiver<()>,
) -> Result<()> {
    gst::init()?;
    let pipeline = gst::Pipeline::new();
    let src = gst::ElementFactory::make("pipewiresrc")
        .name("source")
        .build()
        .map_err(|_| eyre!("failed to create pipewiresrc"))?;
    src.set_property_from_str("path", &node_id.to_string());
    pipeline.add(&src).map_err(|_| eyre!("add pipewiresrc"))?;

    // Report the desktop's *actual* frame size (from the negotiated pipewiresrc caps) so touch can
    // be scaled to it - that's the coordinate space the car's view maps into, regardless of what
    // the portal reported or how the desktop is scaled.
    if let Some(pad) = src.static_pad("src") {
        pad.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |_pad, info| {
            if let Some(gst::PadProbeData::Event(ev)) = &info.data {
                if let gst::EventView::Caps(caps) = ev.view() {
                    if let Some(s) = caps.caps().structure(0) {
                        if let (Ok(w), Ok(h)) = (s.get::<i32>("width"), s.get::<i32>("height")) {
                            desktop_size_tx.send_if_modified(|cur| {
                                let next = Some((w as u32, h as u32));
                                if *cur != next {
                                    tracing::info!(
                                        "desktop stream size {w}x{h} (touch scales to this)"
                                    );
                                    *cur = next;
                                    true
                                } else {
                                    false
                                }
                            });
                        }
                    }
                }
            }
            gst::PadProbeReturn::Ok
        });
    }

    let encoder = build_pipewire_encode(&pipeline, &src, width, height, zerocopy, bitrate)?;
    drive_h264_pipeline(&pipeline, &encoder, video_tx, keyframe_rx).await
}

/// Build the desktop encode chain from `src` (a `pipewiresrc`), scaling/re-timing to the car's
/// `width`x`height` @ 30fps, and return the H264 encoder element. Picks a [`HW_BACKENDS`] row's
/// GPU path when `zerocopy` allows and its elements exist - preferring the backend on the GPU that
/// renders the desktop - else software convert/scale.
fn build_pipewire_encode(
    pipeline: &gst::Pipeline,
    src: &gst::Element,
    width: u32,
    height: u32,
    zerocopy: ZeroCopyMode,
    bitrate: Bitrate,
) -> Result<gst::Element> {
    let mk = |name: &str, label: &str| {
        gst::ElementFactory::make(name)
            .name(label)
            .build()
            .map_err(|_| eyre!("failed to create {label} ({name})"))
    };
    let caps = |feature: Option<&str>| -> gst::Caps {
        let fps = gst::Fraction::new(30, 1);
        match feature {
            Some(f) => gst::Caps::builder("video/x-raw")
                .features([f])
                .field("width", width as i32)
                .field("height", height as i32)
                .field("framerate", fps)
                .build(),
            None => gst::Caps::builder("video/x-raw")
                .field("width", width as i32)
                .field("height", height as i32)
                .field("framerate", fps)
                .build(),
        }
    };

    // A GPU import chain + encoder from the backend's `desktop` elements - Intel/AMD
    // `vapostproc ! vah264enc`, Qualcomm `v4l2convert ! v4l2h264enc`, NVIDIA
    // `glupload ! glcolorconvert ! glcolorscale ! nvh264enc`. The chain imports the portal's
    // dmabuf and does convert + scale on the GPU (`videorate` is memory-agnostic, so it re-times
    // inside the GPU chain), handing GPU memory to the encoder. Genuinely zero-copy for the
    // postproc backends; on NVIDIA the *final* GL->CUDA handoff still downloads to system memory
    // (interop fails with INVALID_GRAPHICS_CONTEXT), but it's the right choice when the desktop
    // *is* NVIDIA-rendered. (For the Intel `va` elements to register on a hybrid box, gst needs
    // `GST_VA_ALL_DRIVERS=1`, which `run_desktop` sets.)
    let build_hw = |b: &HwBackend| -> Result<gst::Element> {
        let (enc_name, encoder) = make_encoder(&b.encoder_desc(bitrate))?;
        let mut chain = b
            .desktop
            .iter()
            .map(|name| mk(name, name))
            .collect::<Result<Vec<_>>>()?;
        chain.push(mk("videorate", "videorate")?);
        pipeline
            .add_many(&chain)
            .map_err(|_| eyre!("add hw elements"))?;
        pipeline.add(&encoder).map_err(|_| eyre!("add encoder"))?;
        gst::Element::link_many(std::iter::once(src).chain(chain.iter()))
            .map_err(|_| eyre!("link hw chain"))?;
        chain
            .last()
            .expect("chain always ends with videorate")
            .link_filtered(&encoder, &caps(Some(b.desktop_memory)))
            .map_err(|_| eyre!("link hw-encoder"))?;
        tracing::info!(
            "Encode source: desktop zero-copy hardware '{}' ({} ! {enc_name}) at {bitrate}",
            b.name,
            b.desktop.join(" ! ")
        );
        Ok(encoder)
    };

    if !matches!(zerocopy, ZeroCopyMode::Off) {
        // Prefer the backend that runs on the GPU the desktop buffer actually lives on - encoding
        // an Intel/AMD desktop on NVIDIA (or vice versa) would be a cross-GPU copy. Sorting rather
        // than filtering keeps the others as fallbacks, in table order.
        let driver = crate::config::desktop_render_driver();
        let mut candidates: Vec<&HwBackend> = HW_BACKENDS.iter().filter(|b| wanted(b)).collect();
        candidates.sort_by_key(|b| !b.matches_driver(driver.as_deref()));
        tracing::info!(
            "desktop renders on GPU driver {}; preferring '{}' encode backend",
            driver.as_deref().unwrap_or("unknown"),
            candidates.first().map(|b| b.name).unwrap_or("none"),
        );
        if let Some(b) = candidates.into_iter().find(|b| b.desktop_available()) {
            return build_hw(b);
        }
        if matches!(zerocopy, ZeroCopyMode::Force) {
            return Err(eyre!(
                "zerocopy=true but no zero-copy desktop backend available (tried: {})",
                HW_BACKENDS
                    .iter()
                    .map(|b| format!("{}: {}", b.name, b.desktop.join(" ! ")))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        tracing::warn!("no zero-copy desktop backend available, using software convert/scale");
    }

    // Software: CPU convert + scale.
    let (enc_name, encoder) = make_encoder(&software_encoder_desc(bitrate)?)?;
    let rate = mk("videorate", "videorate")?;
    let scale = mk("videoscale", "videoscale")?;
    let convert = mk("videoconvert", "videoconvert")?;
    pipeline
        .add_many([&rate, &scale, &convert, &encoder])
        .map_err(|_| eyre!("add sw elements"))?;
    gst::Element::link_many([src, &rate, &scale, &convert]).map_err(|_| eyre!("link sw chain"))?;
    convert
        .link_filtered(&encoder, &caps(None))
        .map_err(|_| eyre!("link convert-encoder"))?;
    tracing::info!("Encode source: desktop software (videoconvert ! {enc_name}) at {bitrate}");
    Ok(encoder)
}

/// Link `encoder` (whose src pad emits H264) -> `h264parse` -> `appsink`, forward each access unit
/// to `video_tx`, set the pipeline PLAYING, and run its bus until EOS/error. The pipeline is torn
/// down when this future is dropped. Shared by the internal-compositor and portal source paths.
async fn drive_h264_pipeline(
    pipeline: &gst::Pipeline,
    encoder: &gst::Element,
    video_tx: mpsc::Sender<EncodedFrame>,
    keyframe_rx: mpsc::Receiver<()>,
) -> Result<()> {
    let mk = |name: &str, label: &str| {
        gst::ElementFactory::make(name)
            .name(label)
            .build()
            .map_err(|_| eyre!("failed to create {label} ({name})"))
    };
    let output_caps = gst::Caps::builder("video/x-h264")
        .field("stream-format", "byte-stream")
        .field("alignment", "au")
        .build();

    let h264parse = mk("h264parse", "h264parse")?;
    h264parse.set_property("config-interval", -1i32);
    // Forced keyframes, asked for by the car (a `ForceKeyFrame` command, or catplay refusing
    // delta frames) over on the AirPlay side. An *upstream* force-key-unit event delivered to
    // the encoder's src pad is the portable way to ask for an IDR: every H264 encoder we use
    // handles it, so this needs no per-backend special-casing. `all_headers` re-sends SPS/PPS
    // with the IDR, which is what lets a car that has lost sync latch back on.
    let _keyframe_pump = {
        let encoder = encoder.clone();
        let mut keyframe_rx = keyframe_rx;
        AbortOnDropHandle::new(tokio::spawn(async move {
            while keyframe_rx.recv().await.is_some() {
                let Some(pad) = encoder.static_pad("src") else {
                    tracing::warn!("Encoder has no src pad; cannot force a keyframe");
                    continue;
                };
                let event = gst_video::UpstreamForceKeyUnitEvent::builder()
                    .all_headers(true)
                    .build();
                if !pad.send_event(event) {
                    tracing::warn!("Encoder refused the force-keyframe event");
                } else {
                    tracing::debug!("Forced a keyframe");
                }
            }
        }))
    };

    // Leaky queue between the encoder and the appsink: if the consumer (`video_tx` -> the car)
    // stalls or is still setting up, drop the *oldest* encoded frames instead of letting them pile
    // up - otherwise a backlog forms (e.g. while the AirPlay connection negotiates) and never
    // drains, showing as a permanent 1-2s delay.
    let sink_queue = mk("queue", "sink_queue")?;
    sink_queue.set_property_from_str("leaky", "downstream");
    sink_queue.set_property("max-size-buffers", 3u32);
    sink_queue.set_property("max-size-time", 0u64);
    sink_queue.set_property("max-size-bytes", 0u32);
    let appsink = mk("appsink", "sink")?;
    appsink.set_property("sync", false);

    pipeline
        .add(&h264parse)
        .map_err(|_| eyre!("add h264parse"))?;
    pipeline
        .add(&sink_queue)
        .map_err(|_| eyre!("add sink_queue"))?;
    pipeline.add(&appsink).map_err(|_| eyre!("add appsink"))?;
    tracing::debug!(?pipeline, "Gstreamer pipeline");
    encoder
        .link(&h264parse)
        .map_err(|_| eyre!("link encoder-parse"))?;
    h264parse
        .link(&sink_queue)
        .map_err(|_| eyre!("link parse-queue"))?;
    sink_queue
        .link_filtered(&appsink, &output_caps)
        .map_err(|_| eyre!("link queue-sink"))?;

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
                match video_tx.blocking_send(EncodedFrame { data: au, config }) {
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

    let bus = pipeline.bus().ok_or_else(|| eyre!("pipeline has no bus"))?;
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

/// How the compositor's rendered dmabuf gets into a backend's chain (internal `source = "app"`
/// path only - the desktop path always starts from `pipewiresrc`).
#[derive(Clone, Copy, PartialEq)]
enum Import {
    /// Pushed as `video/x-raw(memory:DMABuf)` (DMA_DRM) and imported by the backend's `postproc`.
    DmaBuf,
    /// Imported into `GstCudaMemory` by `cuda-dmabuf` (EGL + `cuGraphicsEGLRegisterImage`, which
    /// also de-tiles GPU->GPU). NVIDIA needs this because its block-linear surfaces can *not* be
    /// imported by `glupload`/`vapostproc`; the encoder then consumes CUDA memory with no postproc.
    Cuda,
}

/// A GPU-side zero-copy encode backend: one platform's way of getting frames onto the GPU
/// (`import` / `desktop`) plus the hardware H264 encoder that consumes them, tuned for real time.
/// A backend is selected only when all of its elements exist (probed via `ElementFactory::find`),
/// so a machine without the driver falls through to the next backend, then to software.
///
/// Both video sources share this table: the internal compositor path uses `import` + `postproc`,
/// the desktop (portal/PipeWire) path uses `desktop` + `desktop_memory`. Adding a platform = one
/// row here. Keep the highest-value/most-specific backends first.
///
/// `pub` only because [`EncodeBackend::Hw`] names it; every field is private, so a row stays an
/// implementation detail of this module and callers can only pass the value around.
pub struct HwBackend {
    /// Value for `CARPLAY_ENCODE_BACKEND` to force this backend, and what gets logged.
    name: &'static str,
    /// DRM driver names this backend's GPU uses. The desktop path prefers the backend matching the
    /// GPU that renders the desktop - encoding an Intel/AMD desktop on NVIDIA (or vice versa)
    /// would be a cross-GPU copy.
    drivers: &'static [&'static str],
    /// How the compositor's dmabuf enters the chain.
    import: Import,
    /// Internal path: dmabuf-importing convert element (M2M) between the appsrc and the encoder,
    /// doing RGB->NV12 on-GPU. `None` when the import already yields encoder-ready memory (CUDA).
    /// No scale/convert is needed beyond it - the compositor renders at the car's exact resolution.
    postproc: Option<&'static str>,
    /// Desktop path: elements between `pipewiresrc` and `videorate` that import the portal's
    /// dmabuf and do convert+scale on the GPU (the desktop is *not* the car's resolution).
    desktop: &'static [&'static str],
    /// The memory feature `desktop` hands the encoder (`memory:DMABuf`, `memory:GLMemory`, ...).
    desktop_memory: &'static str,
    /// gst-launch-style hardware encoder description ("name prop=val ..."), tuned for real time:
    /// steady CBR, no B-frame reordering, minimal reference buffering, and a periodic keyframe
    /// interval (the car tears the session down after ~1s without video, and IDRs are large -
    /// emitting them every few frames bursts the car's WiFi). Property names differ per encoder; a
    /// bare name leaves the encoder's defaults. Overridden wholesale by `CARPLAY_ENCODER`.
    ///
    /// One string serves every path this backend appears on - compositor, desktop, and the
    /// system-memory fallback in [`software_encoder_desc`] - so the tuning has to hold whether the
    /// encoder is fed GPU or system memory (see the nvenc note on the `cuda` row).
    encoder: &'static str,
}

/// Probed in order; the first whose elements all exist wins.
const HW_BACKENDS: &[HwBackend] = &[
    // NVIDIA. Internal: `cuda-dmabuf` hands nvh264enc GstCudaMemory directly (it does RGB->NV12
    // itself), so there's no postproc. Desktop: the portal's buffer goes through GL instead -
    // convert/scale on the GPU - because the CUDA importer only takes the compositor's own dmabuf.
    HwBackend {
        name: "cuda",
        drivers: &["nvidia", "nvidia-drm"],
        import: Import::Cuda,
        postproc: None,
        desktop: &["glupload", "glcolorconvert", "glcolorscale"],
        desktop_memory: "memory:GLMemory",
        // preset=p4 is the modern medium preset (used together with tune); ultra-low-latency +
        // bframes=0 + rc-lookahead=0 + zerolatency means no reordering and no lookahead buffering,
        // so frames come out immediately.
        //
        // gop-size=60 (~2s), not the 30 the other rows use, because this one encoder has to serve
        // both memory kinds: fed *system memory* - the desktop GL path, whose final GL->CUDA
        // handoff downloads, and the no-CPU-encoder fallback - nvenc emits an IDR *every frame* at
        // gop-size=30 specifically. It's a quirk of that exact value (30 is fine on the internal
        // path's CUDA memory, and other sizes are fine on system memory), so 60 is the one setting
        // that behaves on every path. Verified via `tests/desktop.rs` on a real GNOME ScreenCast.
        encoder: "nvh264enc preset=p4 tune=ultra-low-latency rc-mode=cbr bitrate={kbps} \
                  gop-size=60 bframes=0 rc-lookahead=0 zerolatency=true aud=true",
    },
    // Qualcomm / embedded (the deployment target). v4l2 stateful encoders import DMABuf via
    // V4L2_MEMORY_DMABUF; rate/keyframe control goes through the driver's V4L2 controls rather than
    // gst properties, so `extra-controls` carries the rate setup the other rows express as encoder
    // properties: `video_bitrate` (V4L2_CID_MPEG_VIDEO_BITRATE, in bit/s) at
    // `video_bitrate_mode=1` (V4L2_MPEG_VIDEO_BITRATE_MODE_CBR - the enum's 0 is VBR), so this row
    // is steady CBR like cuda/vaapi/x264. Keyframe interval stays at the driver's default, to
    // revisit against the real device. Untested off-device: a driver that doesn't implement one of
    // these controls just logs a warning and keeps its own default.
    HwBackend {
        name: "v4l2",
        drivers: &["msm", "msm_drm"],
        import: Import::DmaBuf,
        postproc: Some("v4l2convert"),
        desktop: &["v4l2convert"],
        desktop_memory: "memory:DMABuf",
        encoder: "v4l2h264enc extra-controls=controls,video_bitrate_mode=1,video_bitrate={bps}",
    },
    // Intel / AMD VA-API. vapostproc imports the DMA_DRM dmabuf and outputs NV12 in VAMemory,
    // which vah264enc consumes on the same GPU - fully zero-copy. Defaults use B-frames + non-CBR
    // rate control, which adds reordering/lookahead latency (visible lag) - tune for real-time.
    HwBackend {
        name: "vaapi",
        drivers: &["i915", "xe", "amdgpu", "radeon"],
        import: Import::DmaBuf,
        postproc: Some("vapostproc"),
        desktop: &["vapostproc"],
        desktop_memory: "memory:DMABuf",
        encoder: "vah264enc rate-control=cbr bitrate={kbps} b-frames=0 ref-frames=1 \
                  target-usage=6 key-int-max=30 aud=true",
    },
];

impl HwBackend {
    /// The encoder description for this backend on any path, still holding its bitrate placeholder.
    /// `CARPLAY_ENCODER` wins over the row's tuned default - the override applies to hardware
    /// backends too, not just software.
    fn encoder_template(&self) -> String {
        encoder_override().unwrap_or_else(|| self.encoder.to_string())
    }

    /// The encoder to build for this backend, with the configured `bitrate` substituted in.
    fn encoder_desc(&self, bitrate: Bitrate) -> String {
        bitrate.apply(&self.encoder_template())
    }

    /// Whether this backend's compositor-path elements all exist on this machine.
    fn available(&self) -> bool {
        self.postproc.is_none_or(element_available)
            && element_available(encoder_name(&self.encoder_template()))
    }

    /// Whether this backend's desktop-path elements all exist on this machine.
    fn desktop_available(&self) -> bool {
        self.desktop.iter().copied().all(element_available)
            && element_available(encoder_name(&self.encoder_template()))
    }

    /// Whether this backend runs on the GPU driven by DRM `driver` (used to pick the desktop
    /// backend that lives on the same GPU as the desktop's buffers).
    fn matches_driver(&self, driver: Option<&str>) -> bool {
        driver.is_some_and(|d| self.drivers.contains(&d))
    }

    /// `"name: elem ! elem ! encoder"`, for logging and the no-backend-available error.
    fn describe_elements(&self) -> String {
        let chain = self.postproc.map(|p| format!("{p} ! ")).unwrap_or_default();
        format!("{}: {chain}{}", self.name, encoder_name(self.encoder))
    }
}

/// Explicit encoder override, applied to every path (hardware, desktop and software). It replaces
/// the whole description, so the configured bitrate only reaches it if the override itself spells a
/// `{kbps}`/`{bps}` placeholder.
fn encoder_override() -> Option<String> {
    std::env::var("CARPLAY_ENCODER").ok()
}

/// The factory name of a gst-launch-style encoder description ("nvh264enc gop-size=6" -> "nvh264enc").
fn encoder_name(desc: &str) -> &str {
    desc.split_whitespace().next().unwrap_or(desc)
}

/// Build the encoder element from a gst-launch-style description, returning its factory name too
/// (for logging).
fn make_encoder(desc: &str) -> Result<(String, gst::Element)> {
    let (name, props) = parse_encoder_string(desc)?;
    let encoder = gst::ElementFactory::make(&name)
        .name("encoder")
        .build()
        .map_err(|_| eyre!("failed to create encoder '{name}'"))?;
    for (key, val) in &props {
        encoder.set_property_from_str(key, val);
    }
    Ok((name, encoder))
}

fn element_available(name: &str) -> bool {
    gst::ElementFactory::find(name).is_some()
}

/// Build the encode chain onto `pipeline` (linking from `tail`) for the resolved backend, and
/// return the encoder element (whose src pad is H264) plus the [`FrameFeeder`] describing how to
/// push frames into it.
fn build_backend(
    pipeline: &gst::Pipeline,
    tail: &gst::Element,
    backend: &EncodeBackend,
    bitrate: Bitrate,
) -> Result<(gst::Element, FrameFeeder)> {
    match backend {
        EncodeBackend::Software => Ok((
            build_software_chain(pipeline, tail, bitrate)?,
            FrameFeeder::Shm,
        )),
        EncodeBackend::Hw { backend, importer } => {
            if let Some(importer) = importer {
                // Share our CUDA context with the encoder so the imported GstCudaMemory and the
                // encoder live on the same context. Must be set before the encoder is added to the
                // bin (the bin propagates its contexts to children as they're added).
                pipeline.set_context(&importer.gst_context());
            }
            let encoder = build_hw_chain(pipeline, tail, backend, bitrate)?;
            let feeder = match importer {
                Some(importer) => FrameFeeder::Cuda(importer.clone()),
                None => FrameFeeder::DmaDrm(DmaBufAllocator::new()),
            };
            Ok((encoder, feeder))
        }
    }
}

/// Software encoder default. x264enc is the most widely available H264 encoder (any GStreamer with
/// gst-plugins-ugly), so it's what a machine with no usable GPU path lands on - but its defaults
/// are tuned for *file* encoding and would add seconds of lag here:
/// - `speed-preset=ultrafast` + `tune=zerolatency`: no lookahead, no frame reordering, no threaded
///   frame delay - the encoder emits each frame as it takes it, which is what a live stream needs
///   (and what keeps a CPU encode able to hold 30fps at the car's resolution).
/// - `bframes=0`: B-frames reorder output, trading latency for size. Same reason the hardware
///   backends disable them.
/// - `key-int-max=30`: ~1s keyframes, matching the hardware backends - often enough that the car
///   recovers quickly, rare enough that the IDR bursts don't congest its WiFi.
/// - `pass=cbr` + `bitrate={kbps}` (the configured [`Bitrate`], in kbit/s): a steady rate the link
///   can sustain, not x264's spiky quality-targeted default.
/// - `byte-stream=true` + `aud=true`: Annex-B with access-unit delimiters, which is what the
///   AirPlay screen sink consumes (otherwise h264parse has to repackage every buffer).
const DEFAULT_SW_ENCODER: &str = "x264enc speed-preset=ultrafast tune=zerolatency pass=cbr \
    bitrate={kbps} key-int-max=30 bframes=0 byte-stream=true aud=true";

/// Tried in order when [`DEFAULT_SW_ENCODER`] isn't installed. openh264enc (LGPL, in
/// gst-plugins-bad) is the usual stand-in where gst-plugins-ugly is absent; it's baseline-only, so
/// there's no B-frame reordering to disable - just pin it to a steady bitrate (bits/s here, unlike
/// x264enc's kbit/s - hence `{bps}`) and ~1s keyframes.
const SW_ENCODER_FALLBACKS: &[&str] =
    &["openh264enc complexity=low rate-control=bitrate bitrate={bps} gop-size=30"];

/// The encoder for a software (non-zero-copy) chain, honouring `CARPLAY_ENCODER` first.
///
/// The last resort is a *hardware* encoder fed system memory: `zerocopy=false` rules out
/// zero-copy import, not the GPU's encoder block, and plenty of machines (this project's NVIDIA
/// dev box included) ship a vendor encoder but no CPU one - so falling back to nvh264enc/vah264enc
/// beats failing outright. Each row's `encoder` is tuned to hold up on system memory too (see the
/// nvenc gop-size note), so it can be used here as-is.
fn software_encoder_desc(bitrate: Bitrate) -> Result<String> {
    if let Some(desc) = encoder_override() {
        return Ok(bitrate.apply(&desc));
    }
    let sw = std::iter::once(DEFAULT_SW_ENCODER).chain(SW_ENCODER_FALLBACKS.iter().copied());
    if let Some(desc) = sw.clone().find(|d| element_available(encoder_name(d))) {
        return Ok(bitrate.apply(desc));
    }
    if let Some(desc) = HW_BACKENDS
        .iter()
        .map(|b| b.encoder_desc(bitrate))
        .find(|d| element_available(encoder_name(d)))
    {
        tracing::warn!(
            "no software H264 encoder installed; using hardware '{}' on system memory",
            encoder_name(&desc)
        );
        return Ok(desc);
    }
    Err(eyre!(
        "no H264 encoder available (tried: {})",
        sw.map(encoder_name)
            .chain(HW_BACKENDS.iter().map(|b| encoder_name(b.encoder)))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// Zero-copy hardware chain: `appsrc ! [postproc] ! <encoder>`, per the backend's [`HwBackend`]
/// row. For a `postproc` backend (VA-API / v4l2) the appsrc pushes DMA_DRM and the postproc does
/// RGB->NV12 on the GPU; for CUDA the importer has already produced encoder-ready GstCudaMemory,
/// so the encoder is linked straight to the source. No scale/convert either way - the compositor
/// already renders at the car's exact resolution.
fn build_hw_chain(
    pipeline: &gst::Pipeline,
    src_tail: &gst::Element,
    b: &HwBackend,
    bitrate: Bitrate,
) -> Result<gst::Element> {
    let (enc_name, encoder) = make_encoder(&b.encoder_desc(bitrate))?;
    pipeline.add(&encoder).map_err(|_| eyre!("add encoder"))?;

    let mut tail = src_tail.clone();
    if let Some(name) = b.postproc {
        let postproc = gst::ElementFactory::make(name)
            .name("postproc")
            .build()
            .map_err(|_| eyre!("failed to create postproc ({name})"))?;
        pipeline.add(&postproc).map_err(|_| eyre!("add postproc"))?;
        tail.link(&postproc)
            .map_err(|_| eyre!("link src-postproc"))?;
        tail = postproc;
    }
    tail.link(&encoder).map_err(|_| eyre!("link encoder"))?;

    let chain = b.postproc.map(|p| format!("{p} ! ")).unwrap_or_default();
    tracing::info!(
        "Encode backend: zero-copy hardware '{}' ({chain}{enc_name}) at {bitrate}",
        b.name
    );
    Ok(encoder)
}

/// Software (non-zero-copy) chain: `appsrc(video/x-raw) ! videoconvert ! <encoder>`.
///
/// `videoscale`/`videorate` are intentionally absent: the compositor renders at exactly the
/// car's resolution and the feeder pushes a steady 30fps, so there's nothing to scale or
/// re-time. `videoconvert` stays because most encoders don't accept the compositor's BGRx (e.g.
/// x264enc wants planar YUV) - it does the BGRx->encoder-format conversion on the CPU.
fn build_software_chain(
    pipeline: &gst::Pipeline,
    src_tail: &gst::Element,
    bitrate: Bitrate,
) -> Result<gst::Element> {
    let mk = |name: &str, label: &str| {
        gst::ElementFactory::make(name)
            .name(label)
            .build()
            .map_err(|_| eyre!("failed to create {label} ({name})"))
    };
    let (enc_name, encoder) = make_encoder(&software_encoder_desc(bitrate)?)?;
    let convert = mk("videoconvert", "videoconvert")?;
    pipeline.add(&convert).map_err(|_| eyre!("add convert"))?;
    pipeline.add(&encoder).map_err(|_| eyre!("add encoder"))?;
    src_tail
        .link(&convert)
        .map_err(|_| eyre!("link src-convert"))?;
    convert
        .link(&encoder)
        .map_err(|_| eyre!("link convert-encoder"))?;
    tracing::info!("Encode backend: software (videoconvert ! {enc_name}) at {bitrate}");
    Ok(encoder)
}

/// DMA_DRM caps for a dmabuf - carries the DRM fourcc + modifier so the postproc can import it.
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

/// `video/x-raw(memory:CUDAMemory)` caps matching what the CUDA importer produces (a pitched
/// BGRA buffer). The compositor renders Argb8888, whose little-endian bytes are B,G,R,A = BGRA.
fn cuda_caps(dmabuf: &Dmabuf) -> gst::Caps {
    gst::Caps::builder("video/x-raw")
        .features(["memory:CUDAMemory"])
        .field("format", "BGRA")
        .field("width", dmabuf.width() as i32)
        .field("height", dmabuf.height() as i32)
        .field("framerate", gst::Fraction::new(30, 1))
        .build()
}

/// Extract plane 0 of a dmabuf as a `cuda_dmabuf::DmabufPlane` (the compositor renders a single
/// interleaved RGBA plane). The fd is borrowed for the import and stays owned by the frame.
fn first_plane(dmabuf: &Dmabuf) -> Option<cuda_dmabuf::DmabufPlane> {
    let fd = dmabuf.handles().next()?;
    let offset = dmabuf.offsets().next()?;
    let stride = dmabuf.strides().next()?;
    Some(cuda_dmabuf::DmabufPlane {
        fd: fd.as_raw_fd(),
        width: dmabuf.width(),
        height: dmabuf.height(),
        fourcc: dmabuf.format().code as u32,
        modifier: dmabuf.format().modifier.into(),
        offset,
        stride,
    })
}

/// Wrap a dmabuf's plane fds as a dmabuf-backed `gst::Buffer` with a `VideoMeta`, so the
/// postproc can import it. Fds are dup'd; GStreamer owns its copies.
fn wrap_dmabuf(allocator: &DmaBufAllocator, dmabuf: &Dmabuf) -> Result<gst::Buffer> {
    let (width, height) = (dmabuf.width(), dmabuf.height());
    let mut buffer = gst::Buffer::new();
    let bm = buffer
        .get_mut()
        .ok_or_else(|| eyre!("buffer not uniquely owned"))?;
    let mut offsets = Vec::new();
    let mut strides = Vec::new();
    for (fd, (offset, stride)) in dmabuf.handles().zip(dmabuf.offsets().zip(dmabuf.strides())) {
        let owned = fd
            .as_fd()
            .try_clone_to_owned()
            .map_err(|e| eyre!("dup dmabuf fd: {e}"))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitrate_substitutes_the_unit_each_encoder_wants() {
        let b = Bitrate::from_kbps(3000);
        assert_eq!(b.apply("x264enc bitrate={kbps}"), "x264enc bitrate=3000");
        assert_eq!(
            b.apply("openh264enc bitrate={bps}"),
            "openh264enc bitrate=3000000"
        );
        // No placeholder: left as-is, so a CARPLAY_ENCODER override can pin its own rate.
        assert_eq!(b.apply("x264enc bitrate=8000"), "x264enc bitrate=8000");
    }

    /// Every tuned encoder description has to take the configured bitrate - a row that hardcodes
    /// one (or misspells the placeholder) would silently ignore `[graphics] bitrate`.
    #[test]
    fn every_encoder_takes_the_configured_bitrate() {
        let b = Bitrate::from_kbps(4500);
        for desc in HW_BACKENDS
            .iter()
            .map(|hw| hw.encoder)
            .chain(std::iter::once(DEFAULT_SW_ENCODER))
            .chain(SW_ENCODER_FALLBACKS.iter().copied())
        {
            let applied = b.apply(desc);
            assert_ne!(
                applied,
                desc,
                "'{}' has no {{kbps}}/{{bps}} placeholder, so it ignores the configured bitrate",
                encoder_name(desc)
            );
            assert!(
                !applied.contains('{'),
                "'{applied}' still holds an unsubstituted placeholder"
            );
            // Parseable, and the bitrate landed in a property rather than in the factory name.
            let (name, props) = parse_encoder_string(&applied).unwrap();
            assert_eq!(name, encoder_name(desc));
            assert!(
                props.iter().any(|(_, v)| v.contains("4500")),
                "no property carries the bitrate in '{applied}'"
            );
        }
    }

    /// The v4l2 row passes its whole rate setup as a GstStructure (`extra-controls`), which has to
    /// survive the whitespace-splitting property parser in one piece - commas and all.
    #[test]
    fn v4l2_extra_controls_parse_as_one_property() {
        let v4l2 = HW_BACKENDS.iter().find(|b| b.name == "v4l2").unwrap();
        let (name, props) = parse_encoder_string(&v4l2.encoder_desc(Bitrate::from_kbps(3000)))
            .expect("v4l2 encoder description parses");
        assert_eq!(name, "v4l2h264enc");
        assert_eq!(
            props,
            vec![(
                "extra-controls".to_string(),
                // video_bitrate_mode=1 is CBR, matching the other rows' rate-control=cbr.
                "controls,video_bitrate_mode=1,video_bitrate=3000000".to_string()
            )]
        );
    }
}
