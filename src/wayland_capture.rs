//! Screen capture + touch input driven directly by compositor Wayland protocols
//! (`ext-image-copy-capture-v1` for capture, `wlr-virtual-pointer-unstable-v1` for input),
//! bypassing xdg-desktop-portal entirely. This needs no user interaction (no consent
//! dialog), which matters on a headless embedded device where nobody is present to answer
//! one - but it only works on compositors that implement these protocols (wlroots-based
//! ones like sway/cage, and KWin). GNOME/Mutter implements neither, by design, and must
//! use the xdg-desktop-portal path in `portal.rs` instead.
//!
//! `try_start` returns `None` (not an error) when the compositor is missing a required
//! global, so callers can fall back to the portal.
//!
//! Capture prefers DMA-BUF (GBM-allocated, Linear modifier only - see `pick_dmabuf_format`)
//! over SHM when the compositor offers it, since it avoids the compositor's own GPU readback
//! copy into a CPU-visible buffer - that's the meaningful zero-copy win, and we keep it.
//!
//! The captured frame does still get one CPU copy on our side (mmap + `to_vec()` into a
//! `CapturedFrame`) rather than being handed to GStreamer as dmabuf-backed `GstMemory`. Two
//! reasons this is the right tradeoff, not a shortcut:
//!   1. A true dmabuf-fd-to-encoder zero-copy path can't work with this pipeline: tested live,
//!      `videoscale`/`videorate`/`videoconvert` in this GStreamer build reject `memory:DMABuf`
//!      buffers outright ("transform could not transform ... in anything we support" /
//!      not-negotiated). That would need DRM/VA-aware elements (vapostproc, glupload, v4l2)
//!      the software pipeline doesn't have.
//!   2. `encode.rs` re-pushes the latest frame at a steady 30fps (the compositor only produces
//!      a new frame on damage, but the car needs continuous video). Re-pushing requires a
//!      stable snapshot of the pixels; a live dmabuf mapping would be overwritten by the next
//!      capture into that pool slot. The one copy here is what makes safe re-pushing possible.
//!
//! The re-push itself is zero-copy (an `Arc` refcount bump, see `encode::FrameData`), so we
//! copy once per *captured* frame, not once per *pushed* frame.

use std::{
    ffi::CString,
    os::fd::AsFd,
    sync::Arc,
    time::{Duration, Instant},
};

use eyre::{Result, eyre};
use libcarjack::carplay_session::{TouchEvent, TouchEventType};
use memmap2::{Mmap, MmapMut};
use tokio::sync::watch;
use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle, WEnum,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{
        wl_buffer::WlBuffer,
        wl_output::WlOutput,
        wl_pointer::ButtonState,
        wl_registry::WlRegistry,
        wl_seat::WlSeat,
        wl_shm::{self, WlShm},
        wl_shm_pool::WlShmPool,
    },
};
use wayland_protocols::{
    ext::{
        image_capture_source::v1::client::{
            ext_image_capture_source_v1::ExtImageCaptureSourceV1,
            ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
        },
        image_copy_capture::v1::client::{
            ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1, FailureReason},
            ext_image_copy_capture_manager_v1::{ExtImageCopyCaptureManagerV1, Options},
            ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
        },
    },
    wp::linux_dmabuf::zv1::client::{
        zwp_linux_buffer_params_v1::{self, ZwpLinuxBufferParamsV1},
        zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1,
    },
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};

/// How many GBM buffer objects we cycle through for DMA-BUF capture. Each captured frame's
/// dmabuf fd stays valid indefinitely (the kernel keeps the GEM object alive), but its
/// *content* is only stable until we recapture into the same pool slot - so the pool needs
/// to be deep enough that GStreamer (appsrc queue + encoder) has released frame N before we
/// overwrite slot N again, roughly 4 frames later at 30fps.
const DMABUF_POOL_SIZE: usize = 4;

/// Linux input-event-codes.h `BTN_LEFT`. zwlr-virtual-pointer only models a mouse (there is
/// no standalone "virtual touch" Wayland protocol), so touch down/up is emulated as a
/// left-button press/release at the touch position.
const BTN_LEFT: u32 = 0x110;

/// Target capture rate. Matches the H264 encoder's framerate in `encode.rs`.
const TARGET_FRAME_INTERVAL: Duration = Duration::from_millis(1000 / 30);

/// One captured desktop frame: tightly packed pixels in `gst_format`, row-major, no padding
/// beyond `width * 4` per row - whether captured via SHM or DMA-BUF (see module docs on why
/// DMA-BUF frames are copied into process memory too, rather than handed to GStreamer as
/// dmabuf-backed `GstMemory`).
pub struct CapturedFrame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub gst_format: &'static str,
}

/// A live capture + input session. Frames are published on a `watch` channel (latest frame
/// wins - if the consumer falls behind, older frames are dropped rather than queued, which
/// is the right tradeoff for a live video feed).
pub struct WaylandCapture {
    pub width: u32,
    pub height: u32,
    frame_rx: watch::Receiver<Option<Arc<CapturedFrame>>>,
    conn: Connection,
    virtual_pointer: ZwlrVirtualPointerV1,
    start: Instant,
}

impl WaylandCapture {
    /// Try to set up capture via Wayland protocols. Returns `None` if the compositor is
    /// missing any of the required globals, so the caller can fall back to xdg-desktop-portal.
    pub async fn try_start() -> Option<WaylandCapture> {
        match tokio::task::spawn_blocking(connect_and_setup).await {
            Ok(Ok(capture)) => Some(capture),
            Ok(Err(e)) => {
                tracing::info!(
                    "Wayland capture protocol unavailable, falling back to xdg-desktop-portal: {e}"
                );
                None
            }
            Err(e) => {
                tracing::warn!("Wayland capture setup task panicked: {e}");
                None
            }
        }
    }

    pub fn subscribe_frames(&self) -> watch::Receiver<Option<Arc<CapturedFrame>>> {
        self.frame_rx.clone()
    }

    /// Inject a CarPlay touch event as a virtual pointer click/drag at `(event.x, event.y)`,
    /// treated as pixel coordinates directly in the captured (compositor output) space - the
    /// same assumption `portal.rs::send_touch` makes about the stream's logical coordinate
    /// space. `event.slot` (multi-touch) is ignored: zwlr-virtual-pointer models a single
    /// pointer, so only single-point interactions (tap, drag) work, not multi-touch gestures.
    pub fn send_touch(&self, event: &TouchEvent) -> Result<()> {
        let time = self.start.elapsed().as_millis() as u32;
        let x = (event.x.round() as i64).clamp(0, self.width as i64 - 1) as u32;
        let y = (event.y.round() as i64).clamp(0, self.height as i64 - 1) as u32;

        match event.event_type {
            TouchEventType::Down => {
                self.virtual_pointer
                    .motion_absolute(time, x, y, self.width, self.height);
                self.virtual_pointer.button(time, BTN_LEFT, ButtonState::Pressed);
                self.virtual_pointer.frame();
            }
            TouchEventType::Motion => {
                self.virtual_pointer
                    .motion_absolute(time, x, y, self.width, self.height);
                self.virtual_pointer.frame();
            }
            TouchEventType::Up => {
                self.virtual_pointer.button(time, BTN_LEFT, ButtonState::Released);
                self.virtual_pointer.frame();
            }
        }
        self.conn
            .flush()
            .map_err(|e| eyre!("flush virtual pointer event: {e}"))?;
        Ok(())
    }
}

#[derive(Default)]
struct CaptureState {
    buffer_size: Option<(u32, u32)>,
    shm_formats: Vec<wl_shm::Format>,
    /// Raw `dev_t` of the compositor's preferred DRM device for dmabuf import.
    dmabuf_device: Option<u64>,
    /// (DRM fourcc, supported modifiers) pairs.
    dmabuf_formats: Vec<(u32, Vec<u64>)>,
    session_done: bool,
    session_stopped: bool,
    frame_result: Option<std::result::Result<(), FailureReason>>,
}

impl Dispatch<ExtImageCopyCaptureSessionV1, ()> for CaptureState {
    fn event(
        state: &mut Self,
        _proxy: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_session_v1::Event;
        match event {
            Event::BufferSize { width, height } => state.buffer_size = Some((width, height)),
            Event::ShmFormat {
                format: WEnum::Value(f),
            } => state.shm_formats.push(f),
            Event::DmabufDevice { device } => {
                if let Some(bytes) = device.get(..8) {
                    state.dmabuf_device = Some(u64::from_ne_bytes(bytes.try_into().unwrap()));
                }
            }
            Event::DmabufFormat { format, modifiers } => {
                let modifiers = modifiers
                    .chunks_exact(8)
                    .map(|c| u64::from_ne_bytes(c.try_into().unwrap()))
                    .collect();
                state.dmabuf_formats.push((format, modifiers));
            }
            Event::Done => state.session_done = true,
            Event::Stopped => state.session_stopped = true,
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, ()> for CaptureState {
    fn event(
        state: &mut Self,
        _proxy: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_frame_v1::Event;
        match event {
            Event::Ready => state.frame_result = Some(Ok(())),
            Event::Failed { reason } => {
                let reason = match reason {
                    WEnum::Value(r) => r,
                    WEnum::Unknown(_) => FailureReason::Unknown,
                };
                state.frame_result = Some(Err(reason));
            }
            _ => {}
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for CaptureState {
    fn event(
        _state: &mut Self,
        _proxy: &WlRegistry,
        _event: wayland_client::protocol::wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // registry_queue_init tracks the global list itself; this impl only exists to
        // satisfy the trait bound it requires.
    }
}

wayland_client::delegate_noop!(CaptureState: ignore WlShm);
wayland_client::delegate_noop!(CaptureState: WlShmPool);
wayland_client::delegate_noop!(CaptureState: ignore WlBuffer);
wayland_client::delegate_noop!(CaptureState: ignore WlOutput);
wayland_client::delegate_noop!(CaptureState: ignore WlSeat);
wayland_client::delegate_noop!(CaptureState: ExtImageCaptureSourceV1);
wayland_client::delegate_noop!(CaptureState: ExtOutputImageCaptureSourceManagerV1);
wayland_client::delegate_noop!(CaptureState: ExtImageCopyCaptureManagerV1);
wayland_client::delegate_noop!(CaptureState: ZwlrVirtualPointerManagerV1);
wayland_client::delegate_noop!(CaptureState: ZwlrVirtualPointerV1);
wayland_client::delegate_noop!(CaptureState: ZwpLinuxDmabufV1);
// We only use create_immed (synchronous), so the created/failed events (for the async
// create() request) never fire - ignored rather than asserted-unreachable just in case.
wayland_client::delegate_noop!(CaptureState: ignore ZwpLinuxBufferParamsV1);

fn gst_format_for(format: wl_shm::Format) -> Result<&'static str> {
    // wl_shm's Xrgb8888/Argb8888 are native-endian 32-bit words 0x{X,A}RRGGBB, i.e. in
    // memory (little-endian) byte order B,G,R,{X,A} - GStreamer's "BGRx"/"BGRA" name formats
    // by their in-memory byte order, so that's the equivalent caps format.
    match format {
        wl_shm::Format::Xrgb8888 => Ok("BGRx"),
        wl_shm::Format::Argb8888 => Ok("BGRA"),
        other => Err(eyre!("unsupported shm format {other:?} (only Xrgb8888/Argb8888 supported)")),
    }
}

fn gst_format_for_drm(format: gbm::Format) -> Result<&'static str> {
    match format {
        gbm::Format::Xrgb8888 => Ok("BGRx"),
        gbm::Format::Argb8888 => Ok("BGRA"),
        other => Err(eyre!("unsupported drm format {other:?} (only Xrgb8888/Argb8888 supported)")),
    }
}

/// Pick a capturable format the compositor listed as an importable dmabuf format for this
/// session. We deliberately only ever request the **Linear** modifier, even when the
/// compositor offers others: tested live against this machine's Intel iGPU, handing GBM the
/// *full* advertised modifier list (including Linear) got back
/// `I915_FORMAT_MOD_Y_TILED_GEN12_RC_CCS_CC` - a 3-plane render-compressed layout only Intel's
/// own render/media pipeline can read. Describing a tiled/compressed modifier correctly (via
/// the DRM-generic `format=DMA_DRM` caps) doesn't help either: plain software elements like
/// `videoscale`/`videorate` don't understand `DMA_DRM` at all and reject it outright
/// ("transform could not transform ... in anything we support" / not-negotiated) - confirmed
/// live. Linear is the one modifier that's both consumable and describable with plain
/// `format=<X>` caps. Returns `None` if the compositor didn't advertise a dmabuf device, or
/// none of our supported formats offer Linear - callers should fall back to SHM in that case.
fn pick_dmabuf_format(device: Option<u64>, formats: &[(u32, Vec<u64>)]) -> Option<(u64, gbm::Format)> {
    const DRM_FORMAT_MOD_LINEAR: u64 = 0;
    let device = device?;
    [gbm::Format::Xrgb8888, gbm::Format::Argb8888]
        .into_iter()
        .find(|candidate| {
            formats
                .iter()
                .any(|(f, mods)| *f == *candidate as u32 && mods.contains(&DRM_FORMAT_MOD_LINEAR))
        })
        .map(|format| (device, format))
}

/// Resolve a `dev_t` (as advertised by the compositor) to its DRM *render* node and open it.
/// We deliberately open the render node rather than the primary/card node: it doesn't
/// require DRM master, which our process (not the compositor) won't have.
fn open_render_node(dev: u64) -> Result<std::fs::File> {
    let path = drm::node::dev_path(dev, drm::node::NodeType::Render)
        .map_err(|e| eyre!("no DRM render node for device {dev}: {e}"))?;
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|e| eyre!("open {}: {e}", path.display()))
}

fn connect_and_setup() -> Result<WaylandCapture> {
    let conn = Connection::connect_to_env().map_err(|e| eyre!("connect to Wayland display: {e}"))?;
    let (globals, mut event_queue) =
        registry_queue_init::<CaptureState>(&conn).map_err(|e| eyre!("wl_registry init: {e}"))?;
    let qh = event_queue.handle();

    let list = globals.contents().clone_list();
    let output_global = list
        .iter()
        .find(|g| g.interface == "wl_output")
        .ok_or_else(|| eyre!("compositor has no wl_output"))?;
    let seat_global = list.iter().find(|g| g.interface == "wl_seat");

    let shm: WlShm = globals
        .bind(&qh, 1..=1, ())
        .map_err(|e| eyre!("wl_shm not available: {e}"))?;
    let output: WlOutput = globals
        .registry()
        .bind(output_global.name, output_global.version.min(4), &qh, ());
    let seat: Option<WlSeat> = seat_global
        .map(|g| globals.registry().bind(g.name, g.version.min(1), &qh, ()));
    let source_mgr: ExtOutputImageCaptureSourceManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .map_err(|e| eyre!("ext_output_image_capture_source_manager_v1 not available: {e}"))?;
    let capture_mgr: ExtImageCopyCaptureManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .map_err(|e| eyre!("ext_image_copy_capture_manager_v1 not available: {e}"))?;
    let pointer_mgr: ZwlrVirtualPointerManagerV1 = globals
        .bind(&qh, 1..=2, ())
        .map_err(|e| eyre!("zwlr_virtual_pointer_manager_v1 not available: {e}"))?;
    // Optional: without this, DMA-BUF capture is simply unavailable and we always use SHM.
    let dmabuf_mgr: Option<ZwpLinuxDmabufV1> = globals.bind(&qh, 1..=5, ()).ok();

    let virtual_pointer =
        pointer_mgr.create_virtual_pointer_with_output(seat.as_ref(), Some(&output), &qh, ());

    let mut state = CaptureState::default();
    let source = source_mgr.create_source(&output, &qh, ());
    let session = capture_mgr.create_session(&source, Options::empty(), &qh, ());
    while state.buffer_size.is_none() && !state.session_done && !state.session_stopped {
        event_queue
            .blocking_dispatch(&mut state)
            .map_err(|e| eyre!("wayland dispatch: {e}"))?;
    }
    if state.session_stopped {
        return Err(eyre!("capture session stopped before producing any metadata"));
    }
    let (width, height) = state
        .buffer_size
        .ok_or_else(|| eyre!("compositor never sent buffer_size"))?;

    // Validate at least one capture path is actually usable up front, so a compositor that
    // offers neither DMA-BUF nor a supported SHM format fails here (triggering the portal
    // fallback in main.rs) instead of spawning a background thread doomed to immediately
    // log a warning and exit.
    tracing::debug!(formats = ?state.dmabuf_formats.iter().map(|(f, m)| (format!("{f:08x}"), m.iter().map(|m| format!("{m:#x}")).collect::<Vec<_>>())).collect::<Vec<_>>(), "Wayland capture: compositor dmabuf formats");
    let have_dmabuf = dmabuf_mgr.is_some()
        && pick_dmabuf_format(state.dmabuf_device, &state.dmabuf_formats).is_some();
    if !have_dmabuf {
        pick_format(&state.shm_formats)?;
    } else {
        tracing::info!("Wayland capture: using zero-copy DMA-BUF capture");
    }

    let (frame_tx, frame_rx) = watch::channel(None);
    let conn_for_thread = conn.clone();

    std::thread::Builder::new()
        .name("wayland-capture".into())
        .spawn(move || {
            capture_loop(CaptureLoop {
                conn: conn_for_thread,
                event_queue,
                qh,
                state,
                capture_mgr,
                source_mgr,
                output,
                session,
                shm,
                dmabuf_mgr,
                width,
                height,
                frame_tx,
            })
        })
        .map_err(|e| eyre!("spawn capture thread: {e}"))?;

    Ok(WaylandCapture {
        width,
        height,
        frame_rx,
        conn,
        virtual_pointer,
        start: Instant::now(),
    })
}

fn pick_format(formats: &[wl_shm::Format]) -> Result<wl_shm::Format> {
    formats
        .iter()
        .find(|f| **f == wl_shm::Format::Xrgb8888)
        .or_else(|| formats.iter().find(|f| **f == wl_shm::Format::Argb8888))
        .copied()
        .ok_or_else(|| eyre!("compositor advertised no usable shm format (have {formats:?})"))
}

struct CaptureLoop {
    conn: Connection,
    event_queue: EventQueue<CaptureState>,
    qh: QueueHandle<CaptureState>,
    state: CaptureState,
    capture_mgr: ExtImageCopyCaptureManagerV1,
    source_mgr: ExtOutputImageCaptureSourceManagerV1,
    output: WlOutput,
    session: ExtImageCopyCaptureSessionV1,
    shm: WlShm,
    dmabuf_mgr: Option<ZwpLinuxDmabufV1>,
    width: u32,
    height: u32,
    frame_tx: watch::Sender<Option<Arc<CapturedFrame>>>,
}

enum SessionEnd {
    Stopped,
    BufferConstraintsChanged,
}

enum FrameOutcome {
    Ready,
    Retry,
    SessionEnd(SessionEnd),
}

/// Run one capture cycle (create_frame -> attach_buffer -> capture -> wait for ready/failed)
/// against `buffer`. Shared by both the SHM and DMA-BUF session loops - they only differ in
/// how `buffer` was allocated and how a `Ready` result gets turned into a `CapturedFrame`.
fn capture_one_frame(c: &mut CaptureLoop, buffer: &WlBuffer) -> Result<FrameOutcome> {
    c.state.frame_result = None;
    let frame = c.session.create_frame(&c.qh, ());
    frame.attach_buffer(buffer);
    frame.capture();

    while c.state.frame_result.is_none() && !c.state.session_stopped {
        c.event_queue
            .blocking_dispatch(&mut c.state)
            .map_err(|e| eyre!("wayland dispatch: {e}"))?;
    }
    frame.destroy();

    if c.state.session_stopped {
        return Ok(FrameOutcome::SessionEnd(SessionEnd::Stopped));
    }
    match c.state.frame_result.take() {
        Some(Ok(())) => Ok(FrameOutcome::Ready),
        Some(Err(FailureReason::BufferConstraints)) => {
            Ok(FrameOutcome::SessionEnd(SessionEnd::BufferConstraintsChanged))
        }
        _ => {
            std::thread::sleep(Duration::from_millis(50));
            Ok(FrameOutcome::Retry)
        }
    }
}

fn run_shm_session(c: &mut CaptureLoop) -> Result<SessionEnd> {
    let format = pick_format(&c.state.shm_formats)?;
    let gst_format = gst_format_for(format)?;
    let (width, height) = (c.width, c.height);
    let stride = width * 4;
    let size = (stride as usize) * (height as usize);
    let (file, buffer) = make_shm_buffer(&c.shm, &c.qh, width, height, stride, format, size)
        .map_err(|e| eyre!("allocate shm buffer: {e}"))?;
    let mmap = unsafe { MmapMut::map_mut(&file) }.map_err(|e| eyre!("mmap shm buffer: {e}"))?;

    // One in-flight frame at a time, reusing the same buffer (safe - `ready` is the
    // compositor's signal that it's done writing, there's no separate wl_buffer.release to
    // wait for in this protocol).
    loop {
        match capture_one_frame(c, &buffer)? {
            FrameOutcome::Ready => {
                let _ = c.frame_tx.send(Some(Arc::new(CapturedFrame {
                    data: mmap[..size].to_vec(),
                    width,
                    height,
                    gst_format,
                })));
            }
            FrameOutcome::Retry => continue,
            FrameOutcome::SessionEnd(end) => return Ok(end),
        }
        std::thread::sleep(TARGET_FRAME_INTERVAL);
    }
}

fn run_dmabuf_session(
    c: &mut CaptureLoop,
    device: u64,
    format: gbm::Format,
) -> Result<SessionEnd> {
    let dmabuf_mgr = c
        .dmabuf_mgr
        .clone()
        .ok_or_else(|| eyre!("no zwp_linux_dmabuf_v1"))?;
    let (width, height) = (c.width, c.height);
    let gst_format = gst_format_for_drm(format)?;

    let render_node = open_render_node(device)?;
    let gbm = gbm::Device::new(render_node).map_err(|e| eyre!("open gbm device: {e}"))?;

    // Each pool buffer is only single-plane (Linear Xrgb8888/Argb8888 always is) - mmap it
    // once up front and reuse that mapping across every capture into that slot, same as the
    // SHM path does with its one buffer.
    let mut pool = Vec::with_capacity(DMABUF_POOL_SIZE);
    for _ in 0..DMABUF_POOL_SIZE {
        let bo = gbm
            .create_buffer_object_with_modifiers2::<()>(
                width,
                height,
                format,
                std::iter::once(gbm::Modifier::Linear),
                gbm::BufferObjectFlags::RENDERING,
            )
            .map_err(|e| eyre!("allocate dmabuf buffer: {e}"))?;
        let wl_buffer = make_dmabuf_wl_buffer(&dmabuf_mgr, &c.qh, &bo, width, height, format)?;
        let stride = bo.stride_for_plane(0);
        let fd = bo
            .fd_for_plane(0)
            .map_err(|e| eyre!("export plane fd for readback: {e}"))?;
        let mmap = unsafe { Mmap::map(&std::fs::File::from(fd)) }
            .map_err(|e| eyre!("mmap dmabuf: {e}"))?;
        pool.push((wl_buffer, mmap, stride));
    }

    let mut next = 0usize;
    loop {
        let (wl_buffer, mmap, stride) = &pool[next];
        match capture_one_frame(c, wl_buffer)? {
            FrameOutcome::Ready => {
                let _ = c.frame_tx.send(Some(Arc::new(CapturedFrame {
                    data: copy_tightly_packed(mmap, width, height, *stride),
                    width,
                    height,
                    gst_format,
                })));
            }
            FrameOutcome::Retry => continue,
            FrameOutcome::SessionEnd(end) => return Ok(end),
        }
        next = (next + 1) % DMABUF_POOL_SIZE;
        std::thread::sleep(TARGET_FRAME_INTERVAL);
    }
}

/// Copy `mmap` into a tightly packed (no row padding) buffer. GBM-allocated buffers may use a
/// stride wider than `width * 4` (driver-imposed row alignment); SHM buffers never do, since
/// we choose their stride ourselves, but this handles both correctly either way.
fn copy_tightly_packed(mmap: &Mmap, width: u32, height: u32, stride: u32) -> Vec<u8> {
    let row_bytes = (width * 4) as usize;
    let stride = stride as usize;
    let height = height as usize;
    if stride == row_bytes {
        return mmap[..row_bytes * height].to_vec();
    }
    let mut data = Vec::with_capacity(row_bytes * height);
    for row in 0..height {
        let start = row * stride;
        data.extend_from_slice(&mmap[start..start + row_bytes]);
    }
    data
}

fn make_dmabuf_wl_buffer(
    dmabuf_mgr: &ZwpLinuxDmabufV1,
    qh: &QueueHandle<CaptureState>,
    bo: &gbm::BufferObject<()>,
    width: u32,
    height: u32,
    format: gbm::Format,
) -> Result<WlBuffer> {
    let params = dmabuf_mgr.create_params(qh, ());
    let modifier: u64 = bo.modifier().into();
    let modifier_hi = (modifier >> 32) as u32;
    let modifier_lo = (modifier & 0xffff_ffff) as u32;
    for plane in 0..bo.plane_count() {
        let fd = bo
            .fd_for_plane(plane as i32)
            .map_err(|e| eyre!("export plane {plane} fd: {e}"))?;
        params.add(
            fd.as_fd(),
            plane,
            bo.offset(plane as i32),
            bo.stride_for_plane(plane as i32),
            modifier_hi,
            modifier_lo,
        );
    }
    let wl_buffer = params.create_immed(
        width as i32,
        height as i32,
        format as u32,
        zwp_linux_buffer_params_v1::Flags::empty(),
        qh,
        (),
    );
    params.destroy();
    Ok(wl_buffer)
}

/// Runs for the lifetime of the process. Recreates the capture session whenever the
/// compositor stops it (e.g. output resize / mode change) or reports buffer constraints
/// changed; gives up (ending the desktop stream) only on an actual connection error. Prefers
/// DMA-BUF capture each time a session is (re)created, falling back to SHM if the compositor
/// doesn't offer a usable dmabuf format or the GBM/dmabuf setup fails.
fn capture_loop(mut c: CaptureLoop) {
    loop {
        let dmabuf_target = c
            .dmabuf_mgr
            .is_some()
            .then(|| pick_dmabuf_format(c.state.dmabuf_device, &c.state.dmabuf_formats))
            .flatten();

        let result = match dmabuf_target {
            Some((device, format)) => run_dmabuf_session(&mut c, device, format).or_else(|e| {
                tracing::warn!(
                    "Wayland capture: DMA-BUF path failed ({e}), falling back to SHM for this session"
                );
                run_shm_session(&mut c)
            }),
            None => run_shm_session(&mut c),
        };

        let end = match result {
            Ok(end) => end,
            Err(e) => {
                tracing::warn!("Wayland capture: {e}");
                return;
            }
        };
        if let SessionEnd::BufferConstraintsChanged = end {
            tracing::debug!("Wayland capture: buffer constraints changed, recreating session");
        }

        // Recreate the session against a fresh source.
        c.session.destroy();
        c.state = CaptureState::default();
        let source = c.source_mgr.create_source(&c.output, &c.qh, ());
        c.session = c.capture_mgr.create_session(&source, Options::empty(), &c.qh, ());
        while c.state.buffer_size.is_none() && !c.state.session_done && !c.state.session_stopped {
            if let Err(e) = c.event_queue.blocking_dispatch(&mut c.state) {
                tracing::warn!("Wayland capture: connection error: {e}");
                return;
            }
        }
        if c.state.session_stopped {
            tracing::warn!("Wayland capture: session stopped repeatedly, giving up");
            return;
        }
        let Some((w, h)) = c.state.buffer_size else {
            tracing::warn!("Wayland capture: compositor never sent buffer_size on retry");
            return;
        };
        c.width = w;
        c.height = h;
        let _ = c.conn.flush();
    }
}

fn make_shm_buffer(
    shm: &WlShm,
    qh: &QueueHandle<CaptureState>,
    width: u32,
    height: u32,
    stride: u32,
    format: wl_shm::Format,
    size: usize,
) -> Result<(std::fs::File, WlBuffer)> {
    let name = CString::new("carjack-capture").expect("static name has no NUL bytes");
    let fd = rustix::fs::memfd_create(name.as_c_str(), rustix::fs::MemfdFlags::CLOEXEC)
        .map_err(|e| eyre!("memfd_create: {e}"))?;
    rustix::fs::ftruncate(&fd, size as u64).map_err(|e| eyre!("ftruncate: {e}"))?;
    let file = std::fs::File::from(fd);
    let pool = shm.create_pool(file.as_fd(), size as i32, qh, ());
    let buffer = pool.create_buffer(0, width as i32, height as i32, stride as i32, format, qh, ());
    pool.destroy();
    Ok((file, buffer))
}
