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
pub struct EncodeBackend {
    kind: BackendKind,
}

#[derive(Clone)]
enum BackendKind {
    Cuda(Arc<cuda_dmabuf::CudaImporter>),
    Dmabuf(&'static HwBackend),
    Software,
}

impl EncodeBackend {
    /// Whether this backend consumes the compositor's dmabuf directly (so the compositor must
    /// forward dmabufs); false means the software path consuming CPU-read-back frames.
    pub fn is_zero_copy(&self) -> bool {
        !matches!(self.kind, BackendKind::Software)
    }

    /// Short human-readable name of the selected backend, for logging.
    pub fn describe(&self) -> String {
        match &self.kind {
            BackendKind::Cuda(_) => "zero-copy NVIDIA CUDA".into(),
            BackendKind::Dmabuf(b) => format!("zero-copy hardware '{}'", b.name),
            BackendKind::Software => "software".into(),
        }
    }
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
        ZeroCopyMode::Off => Ok(EncodeBackend {
            kind: BackendKind::Software,
        }),
        ZeroCopyMode::Force => match try_zero_copy(render_node) {
            Some(kind) => Ok(EncodeBackend { kind }),
            None => Err(eyre!(
                "zerocopy=true but no zero-copy backend is available on {render_node} \
                 (need nvh264enc, or vapostproc+vah264enc, or v4l2convert+v4l2h264enc)"
            )),
        },
        ZeroCopyMode::Auto => match try_zero_copy(render_node) {
            Some(kind) => Ok(EncodeBackend { kind }),
            None => {
                tracing::warn!(
                    "zerocopy=auto: no zero-copy backend available on {render_node}, \
                     falling back to SOFTWARE encode"
                );
                Ok(EncodeBackend {
                    kind: BackendKind::Software,
                })
            }
        },
    }
}

/// Try to construct a zero-copy backend for `render_node`. Honours an explicit
/// `CARPLAY_ENCODE_BACKEND` (`cuda`/`vaapi`/`v4l2`) override, else probes NVIDIA (CUDA) first,
/// then the dmabuf-importing HW backends. Returns `None` if none are usable.
fn try_zero_copy(render_node: &str) -> Option<BackendKind> {
    let forced = std::env::var("CARPLAY_ENCODE_BACKEND").ok();
    let want = |name: &str| {
        forced
            .as_deref()
            .is_none_or(|f| f.eq_ignore_ascii_case(name))
    };

    if want("cuda") && element_available("nvh264enc") {
        match cuda_dmabuf::CudaImporter::new(render_node) {
            Ok(imp) => return Some(BackendKind::Cuda(Arc::new(imp))),
            Err(e) => tracing::warn!("NVIDIA/CUDA zero-copy unavailable on {render_node}: {e}"),
        }
    }
    HW_BACKENDS
        .iter()
        .find(|b| want(b.name) && element_available(b.postproc) && element_available(b.encoder))
        .map(BackendKind::Dmabuf)
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
    let (encoder, feeder) = build_backend(&pipeline, &queue, &backend)?;
    tracing::info!("Encode backend: {}", backend.describe());

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
    drive_h264_pipeline(&pipeline, &encoder, video_tx).await
}

/// Stream the user's existing desktop, captured via the xdg-desktop-portal ScreenCast PipeWire
/// `node_id`, scaled/re-timed to the car's `width`x`height` @ 30fps. Used for
/// `source = "desktop"`; input is injected back through the portal (see `src/portal.rs`), not
/// here. `pipewiresrc` is a live source with its own timestamps, so no appsrc/feeder is needed.
///
/// `zerocopy` selects the convert/scale path:
/// - **GL zero-copy** (`Auto`/`Force`, when `glupload` is present): `pipewiresrc ! glupload !
///   glcolorconvert ! glcolorscale ! videorate ! nvh264enc` - imports the portal's dmabuf into a
///   GL texture and does convert/scale/retime on the GPU (`videorate` is memory-agnostic), then
///   feeds the encoder via GL memory (GL-CUDA interop for NVENC). Needs a GL-memory-capable
///   encoder (nvh264enc); the GL context is auto-negotiated.
/// - **Software** (`Off`, or `Auto` without `glupload`): `pipewiresrc ! videorate ! videoscale !
///   videoconvert ! encoder` - convert/scale on the CPU.
pub async fn encode_pipewire_to_channel(
    node_id: u32,
    width: u32,
    height: u32,
    zerocopy: ZeroCopyMode,
    video_tx: mpsc::Sender<EncodedFrame>,
    desktop_size_tx: tokio::sync::watch::Sender<Option<(u32, u32)>>,
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

    let encoder = build_pipewire_encode(&pipeline, &src, width, height, zerocopy)?;
    drive_h264_pipeline(&pipeline, &encoder, video_tx).await
}

/// Build the desktop encode chain from `src` (a `pipewiresrc`), scaling/re-timing to the car's
/// `width`x`height` @ 30fps, and return the H264 encoder element. Picks a GPU zero-copy path when
/// `zerocopy` allows and one is available - GL->NVENC on NVIDIA, or a dmabuf-importing postproc +
/// vendor encoder from `HW_BACKENDS` for VA-API / v4l2 - else software convert/scale.
fn build_pipewire_encode(
    pipeline: &gst::Pipeline,
    src: &gst::Element,
    width: u32,
    height: u32,
    zerocopy: ZeroCopyMode,
) -> Result<gst::Element> {
    let mk = |name: &str, label: &str| {
        gst::ElementFactory::make(name)
            .name(label)
            .build()
            .map_err(|_| eyre!("failed to create {label} ({name})"))
    };
    // The configured (default) encoder - used by the GL and software paths (the HW backends bring
    // their own vendor encoder).
    let default_encoder = || -> Result<(String, gst::Element)> {
        let desc = std::env::var("CARPLAY_ENCODER")
            .unwrap_or_else(|_| String::from(DEFAULT_ENCODER_DESKTOP));
        let (name, props) = parse_encoder_string(&desc)?;
        let enc = mk(&name, "encoder")?;
        for (k, v) in &props {
            enc.set_property_from_str(k, v);
        }
        Ok((name, enc))
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

    // A dmabuf-importing postproc + encoder on the *same* GPU as the desktop - Intel/AMD
    // `vapostproc ! vah264enc`, Qualcomm `v4l2convert ! v4l2h264enc`. The postproc imports the
    // portal's dmabuf and does convert + scale on the GPU, handing dmabuf to the encoder: genuine
    // zero-copy. (For the Intel `va` elements to register on a hybrid box, gst needs
    // `GST_VA_ALL_DRIVERS=1`, which `run_desktop` sets.) Returns `None` if no HW backend exists.
    let build_hw = || -> Result<Option<gst::Element>> {
        let Some(b) = HW_BACKENDS
            .iter()
            .find(|b| element_available(b.postproc) && element_available(b.encoder))
        else {
            return Ok(None);
        };
        let postproc = mk(b.postproc, "postproc")?;
        let rate = mk("videorate", "videorate")?;
        let encoder = mk(b.encoder, "encoder")?;
        for (k, v) in b.props {
            encoder.set_property_from_str(k, v);
        }
        pipeline
            .add_many([&postproc, &rate, &encoder])
            .map_err(|_| eyre!("add hw elements"))?;
        gst::Element::link_many([src, &postproc, &rate]).map_err(|_| eyre!("link hw chain"))?;
        rate.link_filtered(&encoder, &caps(Some("memory:DMABuf")))
            .map_err(|_| eyre!("link hw-encoder"))?;
        tracing::info!(
            "Encode source: desktop ZERO-COPY hardware '{}' ({} ! {})",
            b.name,
            b.postproc,
            b.encoder
        );
        Ok(Some(encoder))
    };
    // NVIDIA/GL: upload to GL and convert/scale/re-time on the GPU, then hand GL memory to
    // `nvh264enc`. NB: the *final* GL->CUDA handoff currently downloads to system memory (interop
    // fails with INVALID_GRAPHICS_CONTEXT), so it's not fully zero-copy - but it's the right choice
    // when the desktop *is* NVIDIA-rendered (same GPU). Returns `None` if the elements are absent.
    let build_gl = || -> Result<Option<gst::Element>> {
        if !(element_available("glupload") && element_available("nvh264enc")) {
            return Ok(None);
        }
        let (enc_name, encoder) = default_encoder()?;
        let glupload = mk("glupload", "glupload")?;
        let glconvert = mk("glcolorconvert", "glconvert")?;
        let glscale = mk("glcolorscale", "glscale")?;
        let rate = mk("videorate", "videorate")?;
        pipeline
            .add_many([&glupload, &glconvert, &glscale, &rate, &encoder])
            .map_err(|_| eyre!("add gl elements"))?;
        gst::Element::link_many([src, &glupload, &glconvert, &glscale, &rate])
            .map_err(|_| eyre!("link gl chain"))?;
        rate.link_filtered(&encoder, &caps(Some("memory:GLMemory")))
            .map_err(|_| eyre!("link gl-encoder"))?;
        tracing::info!("Encode source: desktop GL GPU convert/scale (glupload ! ... ! {enc_name})");
        Ok(Some(encoder))
    };

    if !matches!(zerocopy, ZeroCopyMode::Off) {
        // Pick the backend that runs on the GPU the desktop buffer actually lives on: NVENC only if
        // the desktop is NVIDIA-rendered (encoding an Intel/AMD desktop on NVIDIA would be a
        // cross-GPU copy), otherwise the dmabuf postproc (VA-API/v4l2) on that GPU. Fall through to
        // the other if the preferred one isn't available.
        let driver = crate::config::desktop_render_driver();
        let is_nvidia = matches!(driver.as_deref(), Some("nvidia") | Some("nvidia-drm"));
        tracing::info!(
            "desktop renders on GPU driver {}; preferring {} encode backend",
            driver.as_deref().unwrap_or("unknown"),
            if is_nvidia { "GL/NVENC" } else { "VA-API/v4l2" },
        );
        let attempts: [&dyn Fn() -> Result<Option<gst::Element>>; 2] = if is_nvidia {
            [&build_gl, &build_hw]
        } else {
            [&build_hw, &build_gl]
        };
        for attempt in attempts {
            if let Some(encoder) = attempt()? {
                return Ok(encoder);
            }
        }
        if matches!(zerocopy, ZeroCopyMode::Force) {
            return Err(eyre!(
                "zerocopy=true but no zero-copy desktop backend available (need \
                 vapostproc+vah264enc, v4l2convert+v4l2h264enc, or glupload+nvh264enc)"
            ));
        }
        tracing::warn!("no zero-copy desktop backend available, using software convert/scale");
    }

    // Software: CPU convert + scale.
    let (enc_name, encoder) = default_encoder()?;
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
    tracing::info!("Encode source: desktop software (videoconvert ! {enc_name})");
    Ok(encoder)
}

/// Link `encoder` (whose src pad emits H264) -> `h264parse` -> `appsink`, forward each access unit
/// to `video_tx`, set the pipeline PLAYING, and run its bus until EOS/error. The pipeline is torn
/// down when this future is dropped. Shared by the internal-compositor and portal source paths.
async fn drive_h264_pipeline(
    pipeline: &gst::Pipeline,
    encoder: &gst::Element,
    video_tx: mpsc::Sender<EncodedFrame>,
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

/// A GPU-side zero-copy encode backend: a dmabuf-importing post-processor (RGB->NV12 on the GPU,
/// no CPU copy) feeding a hardware H264 encoder. Selected only when *both* its elements exist
/// (probed via `ElementFactory::find`), so a machine without the driver falls through to the
/// next backend / software. NVIDIA is *not* here - it needs the `cuda-dmabuf` importer because
/// its block-linear surfaces can't be imported by `glupload`/`vapostproc`.
///
/// Adding a platform = one row here. Keep the highest-value/most-specific backends first.
struct HwBackend {
    /// Value for `CARPLAY_ENCODE_BACKEND` to force this backend, and what gets logged.
    name: &'static str,
    /// dmabuf-importing convert+scale element (M2M): RGB->NV12 on-GPU, zero-copy.
    postproc: &'static str,
    /// hardware H264 encoder consuming the post-processor's (NV12, GPU-memory) output.
    encoder: &'static str,
    /// Real-time encoder tuning applied in order: steady CBR, no B-frame reordering, minimal
    /// reference buffering, and a ~1s keyframe interval (the car tears the session down after ~1s
    /// without a keyframe). Property names differ per encoder; empty = leave the encoder defaults.
    props: &'static [(&'static str, &'static str)],
}

/// Probed in order; first whose `postproc` + `encoder` both exist wins.
const HW_BACKENDS: &[HwBackend] = &[
    // Qualcomm / embedded (the deployment target). v4l2 stateful encoders import DMABuf via
    // V4L2_MEMORY_DMABUF; keyframe/rate control is usually via extra-controls, so leave defaults
    // here and revisit against the real device.
    HwBackend {
        name: "v4l2",
        postproc: "v4l2convert",
        encoder: "v4l2h264enc",
        props: &[],
    },
    // Intel / AMD VA-API. vapostproc imports the DMA_DRM dmabuf and outputs NV12 in VAMemory,
    // which vah264enc consumes on the same GPU - fully zero-copy. Defaults use B-frames + non-CBR
    // rate control, which adds reordering/lookahead latency (visible lag) - tune for real-time.
    HwBackend {
        name: "vaapi",
        postproc: "vapostproc",
        encoder: "vah264enc",
        props: &[
            ("rate-control", "cbr"),
            ("bitrate", "12000"),
            ("b-frames", "0"),
            ("ref-frames", "1"),
            ("target-usage", "6"),
            ("key-int-max", "30"),
            ("aud", "true"),
        ],
    },
];

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
) -> Result<(gst::Element, FrameFeeder)> {
    match &backend.kind {
        BackendKind::Software => Ok((build_software_chain(pipeline, tail)?, FrameFeeder::Shm)),
        BackendKind::Cuda(importer) => {
            // Share our CUDA context with nvh264enc so the imported GstCudaMemory and the encoder
            // live on the same context. Must be set before the encoder is added to the bin (the
            // bin propagates its contexts to children as they're added).
            pipeline.set_context(&importer.gst_context());
            let encoder = build_cuda_chain(pipeline, tail)?;
            Ok((encoder, FrameFeeder::Cuda(importer.clone())))
        }
        BackendKind::Dmabuf(b) => {
            let encoder = build_dmabuf_hw_chain(pipeline, tail, b)?;
            Ok((encoder, FrameFeeder::DmaDrm(DmaBufAllocator::new())))
        }
    }
}

const DEFAULT_CUDA_ENCODER: &str = "nvh264enc preset=p4 tune=ultra-low-latency rc-mode=cbr \
    bitrate=12000 gop-size=60 bframes=0 rc-lookahead=0 zerolatency=true aud=true";

const DEFAULT_SW_ENCODER: &std = "";

/// NVIDIA: `appsrc(CUDAMemory) ! nvh264enc`. The importer already produced GstCudaMemory buffers
/// on the encoder's context, so nvh264enc consumes them directly (internal RGB->NV12). No
/// glupload - NVIDIA won't hand its block-linear surface back as a GL texture.
fn build_cuda_chain(pipeline: &gst::Pipeline, src_tail: &gst::Element) -> Result<gst::Element> {
    let enc_desc =
        std::env::var("CARPLAY_ENCODER").unwrap_or_else(|_| String::from(DEFAULT_CUDA_ENCODER));
    let (enc_name, enc_props) = parse_encoder_string(&enc_desc)?;
    let encoder = gst::ElementFactory::make(&enc_name)
        .name("encoder")
        .build()
        .map_err(|_| eyre!("failed to create encoder '{enc_name}'"))?;
    for (key, val) in &enc_props {
        encoder.set_property_from_str(key, val);
    }
    pipeline.add(&encoder).map_err(|_| eyre!("add encoder"))?;
    src_tail
        .link(&encoder)
        .map_err(|_| eyre!("link src-encoder"))?;
    tracing::info!("Encode backend: zero-copy NVIDIA CUDA (dmabuf -> GstCudaMemory ! {enc_name})");
    Ok(encoder)
}

/// VA-API / v4l2: `appsrc(DMABuf) ! <postproc> ! <encoder>`. The postproc imports the DMA_DRM
/// dmabuf and does RGB->NV12 on the GPU; no scale/convert needed (the compositor already renders
/// at the car's resolution).
fn build_dmabuf_hw_chain(
    pipeline: &gst::Pipeline,
    src_tail: &gst::Element,
    b: &HwBackend,
) -> Result<gst::Element> {
    let mk = |name: &str, label: &str| {
        gst::ElementFactory::make(name)
            .name(label)
            .build()
            .map_err(|_| eyre!("failed to create {label} ({name})"))
    };
    let postproc = mk(b.postproc, "postproc")?;
    let encoder = mk(b.encoder, "encoder")?;
    for (k, v) in b.props {
        encoder.set_property_from_str(k, v);
    }
    pipeline.add(&postproc).map_err(|_| eyre!("add postproc"))?;
    pipeline.add(&encoder).map_err(|_| eyre!("add encoder"))?;
    src_tail
        .link(&postproc)
        .map_err(|_| eyre!("link src-postproc"))?;
    postproc
        .link(&encoder)
        .map_err(|_| eyre!("link postproc-encoder"))?;
    tracing::info!(
        "Encode backend: zero-copy hardware '{}' ({} ! {})",
        b.name,
        b.postproc,
        b.encoder
    );
    Ok(encoder)
}

/// Software (non-zero-copy) chain: `appsrc(video/x-raw) ! videoconvert ! <encoder>`.
///
/// `videoscale`/`videorate` are intentionally absent: the compositor renders at exactly the
/// car's resolution and the feeder pushes a steady 30fps, so there's nothing to scale or
/// re-time. `videoconvert` stays because most encoders don't accept the compositor's BGRx (e.g.
/// x264enc wants planar YUV) - it does the BGRx->encoder-format conversion on the CPU.
fn build_software_chain(pipeline: &gst::Pipeline, src_tail: &gst::Element) -> Result<gst::Element> {
    let mk = |name: &str, label: &str| {
        gst::ElementFactory::make(name)
            .name(label)
            .build()
            .map_err(|_| eyre!("failed to create {label} ({name})"))
    };
    let enc_desc =
        std::env::var("CARPLAY_ENCODER").unwrap_or_else(|_| String::from(DEFAULT_SW_ENCODER));
    let (enc_name, enc_props) = parse_encoder_string(&enc_desc)?;
    let encoder = gst::ElementFactory::make(&enc_name)
        .name("encoder")
        .build()
        .map_err(|_| eyre!("failed to create encoder '{enc_name}'"))?;
    for (key, val) in &enc_props {
        encoder.set_property_from_str(key, val);
    }
    let convert = mk("videoconvert", "videoconvert")?;
    pipeline.add(&convert).map_err(|_| eyre!("add convert"))?;
    pipeline.add(&encoder).map_err(|_| eyre!("add encoder"))?;
    src_tail
        .link(&convert)
        .map_err(|_| eyre!("link src-convert"))?;
    convert
        .link(&encoder)
        .map_err(|_| eyre!("link convert-encoder"))?;
    tracing::info!("Encode backend: software (videoconvert ! {enc_name})");
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
