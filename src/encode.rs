use std::os::fd::AsFd;
use std::str::FromStr;
#[cfg(feature = "cuda")]
use std::os::fd::AsRawFd;
use std::sync::Arc;

use catplay_carplay::video::{AnnexBIter, AvccConfig, HevcNalType, NalType};
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
use crate::config::{CodecChoice, ZeroCopyMode};
use libcarjack::airplay::{EncodedFrame, VideoCodec};

/// The configured target video bitrate, in kbit/s. A newtype because encoders disagree on units -
/// most take kbit/s, openh264enc and v4l2 take bit/s - so encoder descriptions spell the unit they
/// want as a `{kbps}`/`{bps}` placeholder and [`Bitrate::apply`] fills it in.
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
    #[cfg(feature = "cuda")]
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
        codec: VideoCodec,
        /// Only on the `cuda` feature: the CUDA context shared across sessions. `Some` exactly for
        /// backends whose [`Import`] is `Cuda`.
        #[cfg(feature = "cuda")]
        importer: Option<Arc<cuda_dmabuf::CudaImporter>>,
    },
    /// CPU path: the compositor reads frames back to system memory and a software encoder (or, if
    /// the machine has none, a hardware one fed system memory) takes them.
    Software { codec: VideoCodec },
}

impl EncodeBackend {
    /// Whether this backend consumes the compositor's dmabuf directly (so the compositor must
    /// forward dmabufs); false means the software path consuming CPU-read-back frames.
    pub fn is_zero_copy(&self) -> bool {
        !matches!(self, Self::Software { .. })
    }

    /// The codec this backend encodes - what the car is told the stream is, and which parameter
    /// sets are pulled out of a keyframe.
    pub fn codec(&self) -> VideoCodec {
        match self {
            Self::Hw { codec, .. } | Self::Software { codec } => *codec,
        }
    }

    /// Short human-readable name of the selected backend, for logging.
    pub fn describe(&self) -> String {
        match self {
            Self::Hw { backend, codec, .. } => {
                format!("zero-copy hardware '{}' {}", backend.name, codec.describe())
            }
            Self::Software { codec } => format!("software {}", codec.describe()),
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
pub fn resolve_backend(
    mode: ZeroCopyMode,
    render_node: &str,
    codec: CodecChoice,
) -> Result<EncodeBackend> {
    let _ = gst::init();
    let codec = resolve_codec(codec);
    match mode {
        ZeroCopyMode::Off => Ok(EncodeBackend::Software { codec }),
        ZeroCopyMode::Force => try_zero_copy(render_node, codec).ok_or_else(|| {
            eyre!(
                "zerocopy=true but no zero-copy backend is available on {render_node} (tried: {})",
                HW_BACKENDS
                    .iter()
                    .map(|b| b.describe_elements(codec))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }),
        ZeroCopyMode::Auto => Ok(try_zero_copy(render_node, codec).unwrap_or_else(|| {
            tracing::warn!(
                "zerocopy=auto: no zero-copy backend available on {render_node}, \
                 falling back to SOFTWARE encode"
            );
            EncodeBackend::Software { codec }
        })),
    }
}

/// Settle the configured codec against what this machine can actually encode.
///
/// `auto` means HEVC: it is what an iPhone sends every unit captured so far, and no accessory has
/// been seen to advertise the choice - the `hevcInfo`/`SUPPORTS_SCREEN_MULTI_CODEC` declarations
/// are absent even on a unit that takes HEVC from a phone, so they can confirm the choice but
/// never rule it out (see `libcarjack::airplay::AirPlayConnection::hevc_declared`). H264 remains a
/// config away for an accessory that turns out to need it.
pub fn resolve_codec(choice: CodecChoice) -> VideoCodec {
    let wanted = match choice {
        CodecChoice::H264 => VideoCodec::H264,
        CodecChoice::Hevc | CodecChoice::Auto => VideoCodec::Hevc,
    };
    if wanted == VideoCodec::H264 || encoder_installed(VideoCodec::Hevc) {
        return wanted;
    }
    tracing::warn!(
        "codec={choice:?} wants HEVC but no HEVC encoder is installed; falling back to H264"
    );
    VideoCodec::H264
}

/// Whether any encoder for `codec` exists here - hardware row or software default.
fn encoder_installed(codec: VideoCodec) -> bool {
    let _ = gst::init();
    HW_BACKENDS.iter().any(|b| b.encodes(codec))
        || sw_encoder_candidates(codec).any(|d| element_available(encoder_name(d)))
}

/// Try to construct a zero-copy backend for `render_node` by probing [`HW_BACKENDS`], honouring an
/// explicit `CARPLAY_ENCODE_BACKEND` override. `None` if none are usable.
///
/// Rows matching the node's actual kernel driver are tried first - a zero-copy import only works
/// within one GPU, so plain table order can reach a backend whose elements probe fine but whose
/// import then fails per-frame on a mismatched GPU. The rest stay as fallbacks, in table order.
fn try_zero_copy(render_node: &str, codec: VideoCodec) -> Option<EncodeBackend> {
    let driver = crate::config::render_node_driver(render_node);
    let mut candidates: Vec<&HwBackend> = HW_BACKENDS
        .iter()
        .filter(|b| wanted(b) && b.available(codec) && b.can_import_from(driver.as_deref()))
        .collect();
    candidates.sort_by_key(|b| !b.matches_driver(driver.as_deref()));
    tracing::info!(
        "{render_node} is driven by {}; usable zero-copy backends: {}",
        driver.as_deref().unwrap_or("an unknown driver"),
        if candidates.is_empty() {
            "none".to_string()
        } else {
            candidates
                .iter()
                .map(|b| b.name)
                .collect::<Vec<_>>()
                .join(", ")
        },
    );

    for backend in candidates {
        match backend.import {
            Import::DmaBuf => {
                return Some(EncodeBackend::Hw {
                    backend,
                    codec,
                    #[cfg(feature = "cuda")]
                    importer: None,
                });
            }
            // CUDA is the one backend that needs state (a CUDA context bound to `render_node`)
            // rather than just elements, so it can fail this late - e.g. the compositor renders on
            // a non-NVIDIA node. Fall through to the next backend if it does.
            #[cfg(feature = "cuda")]
            Import::Cuda => match cuda_dmabuf::CudaImporter::new(render_node) {
                Ok(imp) => {
                    return Some(EncodeBackend::Hw {
                        backend,
                        codec,
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

/// DRM modifiers this backend's postproc can import for Argb8888 (`AR24`); empty means
/// unconstrained. The compositor allocates from this, or GBM picks one the encoder cannot take
/// and negotiation fails only once frames flow.
pub fn importable_modifiers(backend: &EncodeBackend) -> Vec<u64> {
    let EncodeBackend::Hw { backend, .. } = backend else {
        return Vec::new();
    };
    let Some(postproc) = backend.postproc else {
        return Vec::new();
    };
    let _ = gst::init();
    // The element has to outlive the query: a pad whose element has been dropped answers with the
    // template's ANY caps, not the device's real drm-format list.
    let Ok(element) = gst::ElementFactory::make(postproc).build() else {
        return Vec::new();
    };
    let Some(pad) = element.static_pad("sink") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for structure in pad.query_caps(None).iter() {
        let Ok(value) = structure.value("drm-format") else {
            continue;
        };
        if let Ok(one) = value.get::<&str>() {
            collect_argb_modifier(one, &mut out);
        } else if let Ok(list) = value.get::<gst::List>() {
            for item in list.iter() {
                if let Ok(one) = item.get::<&str>() {
                    collect_argb_modifier(one, &mut out);
                }
            }
        }
    }
    out
}

/// `"AR24:0x0100000000000002"` -> that modifier, keeping only the compositor's fourcc. A bare
/// `"AR24"` carries no modifier, which means linear.
fn collect_argb_modifier(drm_format: &str, out: &mut Vec<u64>) {
    let (fourcc, modifier) = match drm_format.split_once(':') {
        Some((f, m)) => (
            f,
            u64::from_str_radix(m.trim_start_matches("0x"), 16).unwrap_or(0),
        ),
        None => (drm_format, 0),
    };
    if fourcc == "AR24" && !out.contains(&modifier) {
        out.push(modifier);
    }
}

/// Whether this build has a usable zero-copy backend for a GPU on `driver`. `auto` render-node
/// selection weighs this above raw GPU capability: without the `cuda` feature nvidia has no encode
/// path here, so preferring an nvidia GPU would rule out zero-copy on a hybrid machine.
pub fn driver_has_zero_copy_backend(driver: &str) -> bool {
    let _ = gst::init();
    HW_BACKENDS
        .iter()
        .any(|b| wanted(b) && b.matches_driver(Some(driver)) && b.encodes(VideoCodec::H264))
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

    let codec = backend.codec();
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

    // Leaky queue: drops the oldest frame (rather than growing latency) if the encoder or the
    // car's WiFi momentarily falls behind.
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
                #[cfg(feature = "cuda")]
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
                            // The plane description is the whole story for an EGL import failure.
                            tracing::warn!(
                                fourcc = %fourcc_str(plane.fourcc),
                                modifier = format!("{:#018x}", plane.modifier),
                                planes = dmabuf.handles().count(),
                                width = plane.width,
                                height = plane.height,
                                stride = plane.stride,
                                offset = plane.offset,
                                "cuda import failed: {e}"
                            );
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
    drive_video_pipeline(&pipeline, &encoder, codec, video_tx, keyframe_rx).await
}

/// Stream the user's existing desktop, captured via the xdg-desktop-portal ScreenCast PipeWire
/// `node_id`, scaled/re-timed to the car's `width`x`height` @ 30fps. Input goes back through the
/// portal (see `src/portal.rs`), not here. `pipewiresrc` is a live source with its own
/// timestamps, so no appsrc/feeder is needed.
///
/// `zerocopy` picks GPU convert/scale (a [`HW_BACKENDS`] row's `desktop` chain, preferring the
/// backend on the GPU that renders the desktop) or software (`videoscale ! videoconvert`).
pub async fn encode_pipewire_to_channel(
    node_id: u32,
    width: u32,
    height: u32,
    zerocopy: ZeroCopyMode,
    codec: CodecChoice,
    bitrate: Bitrate,
    video_tx: mpsc::Sender<EncodedFrame>,
    desktop_size_tx: tokio::sync::watch::Sender<Option<(u32, u32)>>,
    keyframe_rx: mpsc::Receiver<()>,
) -> Result<()> {
    gst::init()?;
    let codec = resolve_codec(codec);
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

    let encoder = build_pipewire_encode(&pipeline, &src, width, height, zerocopy, codec, bitrate)?;
    drive_video_pipeline(&pipeline, &encoder, codec, video_tx, keyframe_rx).await
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
    codec: VideoCodec,
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

    // A GPU import chain + encoder from the backend's `desktop` elements, importing the portal's
    // dmabuf and converting/scaling on the GPU. Genuinely zero-copy for the postproc backends; on
    // NVIDIA the final GL->CUDA handoff still downloads to system memory (interop fails), but it's
    // still the right choice when the desktop is NVIDIA-rendered.
    let build_hw = |b: &HwBackend| -> Result<gst::Element> {
        let desc = b
            .encoder_desc(codec, bitrate)
            .ok_or_else(|| eyre!("'{}' cannot encode {}", b.name, codec.describe()))?;
        let (enc_name, encoder) = make_encoder(&desc)?;
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
        if let Some(b) = candidates.into_iter().find(|b| b.desktop_available(codec)) {
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
    let (enc_name, encoder) = make_encoder(&software_encoder_desc(codec, bitrate)?)?;
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

/// Link `encoder` -> the codec's parser -> `appsink`, forward each access unit to `video_tx`, set
/// the pipeline PLAYING, and run its bus until EOS/error. The pipeline is torn down when this
/// future is dropped. Shared by the internal-compositor and portal source paths.
async fn drive_video_pipeline(
    pipeline: &gst::Pipeline,
    encoder: &gst::Element,
    codec: VideoCodec,
    video_tx: mpsc::Sender<EncodedFrame>,
    keyframe_rx: mpsc::Receiver<()>,
) -> Result<()> {
    let mk = |name: &str, label: &str| {
        gst::ElementFactory::make(name)
            .name(label)
            .build()
            .map_err(|_| eyre!("failed to create {label} ({name})"))
    };
    let (media_type, parser_name) = match codec {
        VideoCodec::H264 => ("video/x-h264", "h264parse"),
        VideoCodec::Hevc => ("video/x-h265", "h265parse"),
    };
    let output_caps = gst::Caps::builder(media_type)
        .field("stream-format", "byte-stream")
        .field("alignment", "au")
        .build();

    let parser = mk(parser_name, parser_name)?;
    parser.set_property("config-interval", -1i32);
    // Forced keyframes, asked for by the car. An upstream force-key-unit event on the encoder's
    // src pad works portably across every backend, no per-backend special-casing needed.
    // `all_headers` re-sends SPS/PPS with the IDR, letting a desynced car latch back on.
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

    // Leaky queue: drops the oldest encoded frames if the consumer stalls or is still setting up,
    // rather than letting an undrained backlog become a permanent delay.
    let sink_queue = mk("queue", "sink_queue")?;
    sink_queue.set_property_from_str("leaky", "downstream");
    sink_queue.set_property("max-size-buffers", 3u32);
    sink_queue.set_property("max-size-time", 0u64);
    sink_queue.set_property("max-size-bytes", 0u32);
    let appsink = mk("appsink", "sink")?;
    appsink.set_property("sync", false);

    pipeline
        .add(&parser)
        .map_err(|_| eyre!("add {parser_name}"))?;
    pipeline
        .add(&sink_queue)
        .map_err(|_| eyre!("add sink_queue"))?;
    pipeline.add(&appsink).map_err(|_| eyre!("add appsink"))?;
    tracing::debug!(?pipeline, "Gstreamer pipeline");
    encoder
        .link(&parser)
        .map_err(|_| eyre!("link encoder-parse"))?;
    parser
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
                let config = extract_codec_config(&au, codec);
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
    #[cfg(feature = "cuda")]
    Cuda,
}

/// A GPU-side zero-copy encode backend: one platform's way of getting frames onto the GPU plus the
/// hardware H264 encoder that consumes them. A backend is selected only when all of its elements
/// exist, so a machine without the driver falls through to the next backend, then to software.
///
/// Both video sources share this table: the compositor path uses `import`/`postproc`, the desktop
/// path uses `desktop`/`desktop_memory`. Adding a platform = one row.
///
/// `pub` only because [`EncodeBackend::Hw`] names it; every field is private.
pub struct HwBackend {
    /// Value for `CARPLAY_ENCODE_BACKEND` to force this backend, and what gets logged.
    name: &'static str,
    /// DRM driver names this backend's GPU uses - the desktop path prefers the backend matching
    /// the GPU that renders the desktop, to avoid a cross-GPU copy.
    drivers: &'static [&'static str],
    /// How the compositor's dmabuf enters the chain.
    import: Import,
    /// Compositor path: dmabuf-importing convert element (RGB->NV12 on-GPU). `None` when the
    /// import already yields encoder-ready memory (CUDA).
    postproc: Option<&'static str>,
    /// Desktop path: elements between `pipewiresrc` and `videorate` that import the portal's
    /// dmabuf and convert+scale on the GPU (the desktop isn't the car's resolution).
    desktop: &'static [&'static str],
    /// The memory feature `desktop` hands the encoder (`memory:DMABuf`, `memory:GLMemory`, ...).
    desktop_memory: &'static str,
    /// gst-launch-style hardware encoder description, tuned for real time (VBR, no B-frames, ~1s
    /// keyframes). One string serves every path this backend appears on - compositor, desktop,
    /// and the system-memory fallback in [`software_encoder_desc`] - so the tuning has to hold on
    /// both GPU and system memory (see the nvenc gop-size note below). `CARPLAY_ENCODER` overrides
    /// it wholesale.
    ///
    /// VBR, not CBR: CBR pads every frame with filler to the exact target, so a static screen costs
    /// as much as a moving one. An iPhone's stream is variable and idles at nothing.
    encoder: &'static str,
    /// The same for HEVC, where this backend has one. `None` means this row cannot encode HEVC and
    /// is skipped when that codec is asked for.
    encoder_hevc: Option<&'static str>,
    /// Caps to hand the encoder on the compositor path, when what the postproc offers first is not
    /// what the car wants. `vah265enc` takes `vapostproc`'s YUY2 and encodes 4:2:2 (`main-422-10`,
    /// which decodes as Range Extensions) unless NV12 is spelled out; a phone sends 8-bit 4:2:0.
    /// The memory feature has to be named too, or negotiation falls back to system memory.
    encoder_input: Option<&'static str>,
}

/// Probed in order; the first whose elements all exist wins.
const HW_BACKENDS: &[HwBackend] = &[
    // NVIDIA. `cuda-dmabuf` hands nvh264enc GstCudaMemory directly on the compositor path (no
    // postproc); the desktop path goes through GL instead, since the CUDA importer only takes the
    // compositor's own dmabuf.
    #[cfg(feature = "cuda")]
    HwBackend {
        name: "cuda",
        drivers: &["nvidia", "nvidia-drm"],
        import: Import::Cuda,
        postproc: None,
        desktop: &["glupload", "glcolorconvert", "glcolorscale"],
        desktop_memory: "memory:GLMemory",
        // gop-size=60, not the 30 other rows use: this encoder also serves system memory (the
        // desktop GL path's GL->CUDA handoff downloads, and the no-CPU-encoder fallback), and
        // nvenc emits an IDR every frame at gop-size=30 specifically when fed system memory. 60 is
        // the one value that behaves on every path this row appears on.
        encoder: "nvh264enc preset=p4 tune=ultra-low-latency rc-mode=vbr bitrate={kbps} \
                  gop-size=60 bframes=0 rc-lookahead=0 zerolatency=true aud=true",
        encoder_hevc: Some(
            "nvh265enc preset=p4 tune=ultra-low-latency rc-mode=vbr bitrate={kbps} \
             gop-size=60 bframes=0 rc-lookahead=0 zerolatency=true aud=true",
        ),
        // The CUDA importer already hands the encoder what it wants.
        encoder_input: None,
    },
    // Qualcomm / embedded (the deployment target). v4l2 stateful encoders take DMABuf directly;
    // rate/keyframe control goes through V4L2 controls (`extra-controls`) rather than gst
    // properties. Untested off-device.
    HwBackend {
        name: "v4l2",
        drivers: &["msm", "msm_drm"],
        import: Import::DmaBuf,
        postproc: Some("v4l2convert"),
        desktop: &["v4l2convert"],
        desktop_memory: "memory:DMABuf",
        // `video_bitrate_mode=0` is VBR in V4L2 (1 is CBR).
        encoder: "v4l2h264enc extra-controls=controls,video_bitrate_mode=0,video_bitrate={bps}",
        encoder_hevc: Some(
            "v4l2h265enc extra-controls=controls,video_bitrate_mode=0,video_bitrate={bps}",
        ),
        encoder_input: Some("video/x-raw(memory:DMABuf), format=NV12"),
    },
    // Intel / AMD VA-API. vapostproc imports the dmabuf and outputs NV12 in VAMemory, which
    // vah264enc consumes on the same GPU - fully zero-copy.
    HwBackend {
        name: "vaapi",
        drivers: &["i915", "xe", "amdgpu", "radeon"],
        import: Import::DmaBuf,
        postproc: Some("vapostproc"),
        desktop: &["vapostproc"],
        desktop_memory: "memory:DMABuf",
        // Under VBR `bitrate` is the ceiling and `target-percentage` (default 66) sets the target.
        encoder: "vah264enc rate-control=vbr bitrate={kbps} b-frames=0 ref-frames=1 \
                  target-usage=6 key-int-max=30 aud=true",
        encoder_hevc: Some(
            "vah265enc rate-control=vbr bitrate={kbps} b-frames=0 ref-frames=1 \
             target-usage=6 key-int-max=30 aud=true",
        ),
        encoder_input: Some("video/x-raw(memory:VAMemory), format=NV12"),
    },
];

impl HwBackend {
    /// The encoder description for this backend on any path, still holding its bitrate placeholder.
    /// `CARPLAY_ENCODER` wins over the row's tuned default - the override applies to hardware
    /// backends too, not just software - and is assumed to match the codec being asked for.
    fn encoder_template(&self, codec: VideoCodec) -> Option<String> {
        if let Some(desc) = encoder_override() {
            return Some(desc);
        }
        match codec {
            VideoCodec::H264 => Some(self.encoder.to_string()),
            VideoCodec::Hevc => self.encoder_hevc.map(str::to_string),
        }
    }

    /// The encoder to build for this backend, with the configured `bitrate` substituted in.
    fn encoder_desc(&self, codec: VideoCodec, bitrate: Bitrate) -> Option<String> {
        self.encoder_template(codec)
            .map(|t| bitrate.apply(&t))
    }

    /// Whether this backend can encode `codec` on this machine at all.
    fn encodes(&self, codec: VideoCodec) -> bool {
        self.encoder_template(codec)
            .is_some_and(|t| element_available(encoder_name(&t)))
    }

    /// Whether this backend's compositor-path elements all exist on this machine.
    fn available(&self, codec: VideoCodec) -> bool {
        self.postproc.is_none_or(element_available) && self.encodes(codec)
    }

    /// Whether this backend's desktop-path elements all exist on this machine.
    fn desktop_available(&self, codec: VideoCodec) -> bool {
        self.desktop.iter().copied().all(element_available) && self.encodes(codec)
    }

    /// Whether this backend runs on the GPU driven by DRM `driver` (used to pick the desktop
    /// backend that lives on the same GPU as the desktop's buffers).
    fn matches_driver(&self, driver: Option<&str>) -> bool {
        driver.is_some_and(|d| self.drivers.contains(&d))
    }

    /// A dmabuf import cannot cross GPUs, and a mismatched one fails only once frames flow - the
    /// pipeline builds, then rejects the caps mid-session. So it must be ruled out here, unlike
    /// `Cuda`, which reports failure up front. Permissive when the driver is unknown.
    fn can_import_from(&self, driver: Option<&str>) -> bool {
        match self.import {
            Import::DmaBuf => driver.is_none() || self.matches_driver(driver),
            #[cfg(feature = "cuda")]
            Import::Cuda => true,
        }
    }

    /// `"name: elem ! elem ! encoder"`, for logging and the no-backend-available error.
    fn describe_elements(&self, codec: VideoCodec) -> String {
        let chain = self.postproc.map(|p| format!("{p} ! ")).unwrap_or_default();
        let enc = self
            .encoder_template(codec)
            .map(|t| encoder_name(&t).to_string())
            .unwrap_or_else(|| format!("no {} encoder", codec.describe()));
        format!("{}: {chain}{enc}", self.name)
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
    let codec = backend.codec();
    match backend {
        EncodeBackend::Software { .. } => Ok((
            build_software_chain(pipeline, tail, codec, bitrate)?,
            FrameFeeder::Shm,
        )),
        EncodeBackend::Hw {
            backend,
            codec: _,
            #[cfg(feature = "cuda")]
            importer,
        } => {
            #[cfg(feature = "cuda")]
            if let Some(importer) = importer {
                // Share our CUDA context with the encoder so the imported GstCudaMemory and the
                // encoder live on the same context. Must be set before the encoder is added to the
                // bin (the bin propagates its contexts to children as they're added).
                pipeline.set_context(&importer.gst_context());
            }
            let encoder = build_hw_chain(pipeline, tail, backend, codec, bitrate)?;
            #[cfg(feature = "cuda")]
            let feeder = match importer {
                Some(importer) => FrameFeeder::Cuda(importer.clone()),
                None => FrameFeeder::DmaDrm(DmaBufAllocator::new()),
            };
            #[cfg(not(feature = "cuda"))]
            let feeder = FrameFeeder::DmaDrm(DmaBufAllocator::new());
            Ok((encoder, feeder))
        }
    }
}

/// Software encoder default. x264enc is the most widely available H264 encoder, but its defaults
/// are tuned for *file* encoding and would add seconds of lag: `ultrafast`/`zerolatency` and
/// `bframes=0` remove reordering/lookahead delay, `key-int-max=30` matches the hardware backends'
/// ~1s keyframes, `pass=qual` is x264's bitrate-targeted VBR (`cbr` would pad a static screen to
/// the full rate), and `byte-stream`/`aud` match what the AirPlay screen sink consumes.
const DEFAULT_SW_ENCODER: &str = "x264enc speed-preset=ultrafast tune=zerolatency pass=qual \
    bitrate={kbps} key-int-max=30 bframes=0 byte-stream=true aud=true";

/// Tried in order when [`DEFAULT_SW_ENCODER`] isn't installed. openh264enc is the usual stand-in
/// where gst-plugins-ugly is absent; baseline-only, so there's no B-frame reordering to disable.
const SW_ENCODER_FALLBACKS: &[&str] =
    &["openh264enc complexity=low rate-control=bitrate bitrate={bps} gop-size=30"];

/// The HEVC equivalents, in the same order of preference.
const DEFAULT_SW_ENCODER_HEVC: &str = "x265enc speed-preset=ultrafast tune=zerolatency \
    bitrate={kbps} key-int-max=30 option-string=bframes=0";
const SW_ENCODER_FALLBACKS_HEVC: &[&str] = &[];

/// Software encoder descriptions to try for `codec`, most preferred first.
fn sw_encoder_candidates(codec: VideoCodec) -> impl Iterator<Item = &'static str> + Clone {
    let (first, rest) = match codec {
        VideoCodec::H264 => (DEFAULT_SW_ENCODER, SW_ENCODER_FALLBACKS),
        VideoCodec::Hevc => (DEFAULT_SW_ENCODER_HEVC, SW_ENCODER_FALLBACKS_HEVC),
    };
    std::iter::once(first).chain(rest.iter().copied())
}

/// The encoder for a software (non-zero-copy) chain, honouring `CARPLAY_ENCODER` first.
///
/// Last resort is a hardware encoder fed system memory: `zerocopy=false` rules out zero-copy
/// import, not the GPU's encoder block, and plenty of machines ship a vendor encoder but no CPU
/// one. Each row's `encoder` is tuned to hold up on system memory too, so it's usable as-is.
fn software_encoder_desc(codec: VideoCodec, bitrate: Bitrate) -> Result<String> {
    if let Some(desc) = encoder_override() {
        return Ok(bitrate.apply(&desc));
    }
    let sw = sw_encoder_candidates(codec);
    if let Some(desc) = sw.clone().find(|d| element_available(encoder_name(d))) {
        return Ok(bitrate.apply(desc));
    }
    if let Some(desc) = HW_BACKENDS
        .iter()
        .filter_map(|b| b.encoder_desc(codec, bitrate))
        .find(|d| element_available(encoder_name(d)))
    {
        tracing::warn!(
            "no software {} encoder installed; using hardware '{}' on system memory",
            codec.describe(),
            encoder_name(&desc)
        );
        return Ok(desc);
    }
    Err(eyre!(
        "no {} encoder available (tried: {})",
        codec.describe(),
        sw.map(|d| encoder_name(d).to_string())
            .chain(
                HW_BACKENDS
                    .iter()
                    .filter_map(|b| b.encoder_template(codec))
                    .map(|t| encoder_name(&t).to_string())
            )
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
    codec: VideoCodec,
    bitrate: Bitrate,
) -> Result<gst::Element> {
    let desc = b
        .encoder_desc(codec, bitrate)
        .ok_or_else(|| eyre!("'{}' cannot encode {}", b.name, codec.describe()))?;
    let (enc_name, encoder) = make_encoder(&desc)?;
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
    match b.encoder_input.map(gst::Caps::from_str).transpose()? {
        Some(caps) => tail
            .link_filtered(&encoder, &caps)
            .map_err(|_| eyre!("link encoder ({caps})"))?,
        None => tail.link(&encoder).map_err(|_| eyre!("link encoder"))?,
    }

    let chain = b.postproc.map(|p| format!("{p} ! ")).unwrap_or_default();
    tracing::info!(
        "Encode backend: zero-copy hardware '{}' ({chain}{enc_name}) at {bitrate}",
        b.name
    );
    Ok(encoder)
}

/// Software (non-zero-copy) chain: `appsrc(video/x-raw) ! videoconvert ! <encoder>`.
///
/// `videoscale`/`videorate` are absent - the compositor already renders at the car's exact
/// resolution at a steady rate. `videoconvert` stays since most encoders reject the compositor's
/// BGRx.
fn build_software_chain(
    pipeline: &gst::Pipeline,
    src_tail: &gst::Element,
    codec: VideoCodec,
    bitrate: Bitrate,
) -> Result<gst::Element> {
    let mk = |name: &str, label: &str| {
        gst::ElementFactory::make(name)
            .name(label)
            .build()
            .map_err(|_| eyre!("failed to create {label} ({name})"))
    };
    let (enc_name, encoder) = make_encoder(&software_encoder_desc(codec, bitrate)?)?;
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
#[cfg(feature = "cuda")]
fn cuda_caps(dmabuf: &Dmabuf) -> gst::Caps {
    gst::Caps::builder("video/x-raw")
        .features(["memory:CUDAMemory"])
        .field("format", "BGRA")
        .field("width", dmabuf.width() as i32)
        .field("height", dmabuf.height() as i32)
        .field("framerate", gst::Fraction::new(30, 1))
        .build()
}

/// A DRM fourcc as its four characters, for logs.
#[cfg(feature = "cuda")]
fn fourcc_str(fourcc: u32) -> String {
    String::from_utf8_lossy(&fourcc.to_le_bytes()).into_owned()
}

/// Extract plane 0 of a dmabuf as a `cuda_dmabuf::DmabufPlane` (the compositor renders a single
/// interleaved RGBA plane). The fd is borrowed for the import and stays owned by the frame.
#[cfg(feature = "cuda")]
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

/// Extract the parameter sets from a keyframe's Annex-B access unit: SPS/PPS for H264,
/// VPS/SPS/PPS for HEVC.
///
/// They stay Annex-B with their start codes, in decode order, which is the form catplay's
/// `avcc_config_serialize`/`hvcc_config_serialize` parse to build the config frame's atom.
fn extract_codec_config(data: &[u8], codec: VideoCodec) -> Option<AvccConfig> {
    let mut sets: [Vec<u8>; 3] = Default::default();

    for chunk in AnnexBIter::annexb(data) {
        let chunk = chunk.ok()?;
        if chunk.data_size == 0 {
            continue;
        }
        let nal_start = chunk.prefix_start + chunk.prefix_len;
        let slot = match codec {
            VideoCodec::H264 => match NalType::from_byte(data[nal_start]) {
                NalType::Sps => 1,
                NalType::Pps => 2,
                _ => continue,
            },
            VideoCodec::Hevc => match HevcNalType::from_byte(data[nal_start]) {
                HevcNalType::VpsNut => 0,
                HevcNalType::SpsNut => 1,
                HevcNalType::PpsNut => 2,
                _ => continue,
            },
        };
        sets[slot].extend_from_slice(&data[chunk.prefix_start..nal_start + chunk.data_size]);
    }

    // No SPS means this is not the keyframe that carries the configuration.
    if sets[1].is_empty() {
        return None;
    }
    if codec.is_hevc() && sets[0].is_empty() {
        return None;
    }

    Some(AvccConfig {
        nal_size_len: 4,
        sps_pps: sets.concat(),
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
    /// The head of a real `vah265enc` access unit (`/tmp/enc-hevc3.h265`, 800x1280 Main): an AUD,
    /// then VPS/SPS/PPS, then the IDR slice. Truncated - only the parameter sets matter here.
    const HEVC_KEYFRAME_HEAD: &[&[u8]] = &[
        &[0x46, 0x01, 0x10],                                     // 35, AUD
        &[0x40, 0x01, 0x0c, 0x01, 0xff, 0xff, 0x01, 0x60],       // 32, VPS
        &[0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03],       // 33, SPS
        &[0x44, 0x01, 0xc0, 0x76, 0x4c, 0x08, 0x90],             // 34, PPS
        &[0x26, 0x01, 0xaf, 0xe0, 0xfa, 0x20, 0xab, 0x43],       // 19, IDR slice
    ];

    fn annexb(nals: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for nal in nals {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(nal);
        }
        out
    }

    /// HEVC needs VPS as well as SPS/PPS, and its NAL type lives in different bits than H264's -
    /// reading it the H264 way finds nothing and the car gets a config frame with no parameter
    /// sets at all.
    #[test]
    fn hevc_config_collects_vps_sps_pps_in_order() {
        let au = annexb(HEVC_KEYFRAME_HEAD);
        let config = super::extract_codec_config(&au, VideoCodec::Hevc).expect("config extracted");
        assert_eq!(config.nal_size_len, 4);
        assert_eq!(
            config.sps_pps,
            annexb(&HEVC_KEYFRAME_HEAD[1..4]),
            "expected VPS, SPS then PPS, with their start codes and nothing else"
        );
        // The same bytes read as H264 are not parameter sets, so nothing should come back.
        assert!(super::extract_codec_config(&au, VideoCodec::H264).is_none());
    }

    /// A frame that carries no parameter sets is a delta frame, and must not be announced as a
    /// keyframe carrying configuration.
    #[test]
    fn a_frame_without_parameter_sets_has_no_config() {
        let au = annexb(&[&[0x02, 0x01, 0xd0, 0x09]]); // type 1, TrailR
        assert!(super::extract_codec_config(&au, VideoCodec::Hevc).is_none());
    }

    use super::*;

    #[test]
    fn argb_modifiers_are_parsed_and_filtered_by_fourcc() {
        let mut out = Vec::new();
        for s in [
            "NV12:0x0100000000000002",
            "AR24:0x0100000000000002",
            "YU12",
            "AR24",                    // no modifier means linear
            "AR24:0x0100000000000002", // deduped
            "AB24:0x1",
        ] {
            super::collect_argb_modifier(s, &mut out);
        }
        assert_eq!(out, vec![0x0100000000000002, 0]);
    }

    /// Software imposes no constraint, so the compositor keeps its full modifier set.
    #[test]
    fn software_backend_is_unconstrained() {
        assert!(
            super::importable_modifiers(&EncodeBackend::Software {
                codec: VideoCodec::Hevc
            })
            .is_empty()
        );
    }

    /// Machine-gated. This returned empty once because the element was dropped before its pad was
    /// queried, and an orphaned pad answers with template ANY caps instead of the real list.
    #[test]
    fn an_available_dmabuf_backend_reports_its_modifiers() {
        let _ = gst::init();
        let Some(hw) = HW_BACKENDS
            .iter()
            .find(|b| {
                b.import == Import::DmaBuf && b.postproc.is_some() && b.available(VideoCodec::Hevc)
            })
        else {
            return;
        };
        let backend = EncodeBackend::Hw {
            backend: hw,
            codec: VideoCodec::Hevc,
            #[cfg(feature = "cuda")]
            importer: None,
        };
        assert!(
            !super::importable_modifiers(&backend).is_empty(),
            "'{}' reported no importable modifiers",
            hw.name
        );
    }

    /// A dmabuf backend on the wrong GPU used to be selectable, then died mid-session on
    /// `caps ... not accepted` - vaapi picked for an NVIDIA render node because its elements
    /// exist for a second, Intel GPU.
    #[test]
    fn dmabuf_backends_are_confined_to_their_own_gpu() {
        for b in HW_BACKENDS.iter().filter(|b| b.import == Import::DmaBuf) {
            assert!(
                !b.can_import_from(Some("nvidia")),
                "'{}' must not be offered for an nvidia render node",
                b.name
            );
            assert!(b.can_import_from(Some(b.drivers[0])), "'{}'", b.name);
            // Unknown driver stays permissive - there is nothing better to go on.
            assert!(b.can_import_from(None), "'{}'", b.name);
        }
    }

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
        let desc = v4l2
            .encoder_desc(VideoCodec::H264, Bitrate::from_kbps(3000))
            .expect("the v4l2 row has an H264 encoder");
        let (name, props) =
            parse_encoder_string(&desc).expect("v4l2 encoder description parses");
        assert_eq!(name, "v4l2h264enc");
        assert_eq!(
            props,
            vec![(
                "extra-controls".to_string(),
                // video_bitrate_mode=0 is VBR, matching the other rows' rate-control=vbr.
                "controls,video_bitrate_mode=0,video_bitrate=3000000".to_string()
            )]
        );
    }
}
