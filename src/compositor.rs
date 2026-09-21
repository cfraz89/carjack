//! Internal Wayland compositor (Smithay-based). carjack runs its own minimal headless compositor
//! so it can render at exactly the accessory's resolution, render on the same GPU it encodes on
//! (zero-copy), pull composited frames straight out of the render loop, and inject CarPlay touch
//! directly into the seat.
//!
//! Renders on its own 30fps timer rather than waiting on client damage, so a static desktop still
//! emits frames. Runs its calloop event loop on a dedicated thread (the GLES/EGL context is
//! thread-bound and `!Send`), publishing frames on a `watch` channel and taking touch on a calloop
//! channel.

use std::ffi::OsString;
use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::config::ZeroCopyMode;
use crate::encode::FrameFormat;
use crate::nv12::Nv12Converter;
use crate::writeback::WritebackConverter;
use eyre::{Result, eyre};
use libcarjack::carplay_init::{TouchEvent, TouchEventType};
use smithay::backend::allocator::dmabuf::{AsDmabuf, Dmabuf};
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::allocator::{Allocator, Fourcc};
use smithay::backend::drm::DrmDeviceFd;
use smithay::backend::egl::{EGLContext, EGLDisplay};
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::on_commit_buffer_handler;
use smithay::backend::renderer::{Bind, ExportMem, ImportDma};
use smithay::desktop::{PopupManager, Space, Window, WindowSurfaceType};
use smithay::input::touch::{DownEvent, MotionEvent, UpEvent};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{
    self, EventLoop, Interest, LoopSignal, Mode as CalloopMode, PostAction,
};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::wl_buffer;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, Display, DisplayHandle};
use smithay::utils::{Buffer as BufferCoord, Logical, Point, Rectangle, Size, Transform};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    CompositorClientState, CompositorHandler, CompositorState, get_parent, is_sync_subsurface,
};
use smithay::wayland::dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier};
use smithay::wayland::output::{OutputHandler, OutputManagerState};
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::selection::data_device::{
    ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
    set_data_device_focus,
};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::wayland::socket::ListeningSocketSource;
use smithay::{
    delegate_compositor, delegate_data_device, delegate_dmabuf, delegate_output, delegate_seat,
    delegate_shm, delegate_xdg_shell,
};

/// One composited desktop frame for the encoder.
pub enum CapturedFrame {
    /// System-memory pixels read back from the GPU (the software encode path). Tightly packed,
    /// `gst_format` layout.
    Shm {
        data: Vec<u8>,
        width: u32,
        height: u32,
        gst_format: &'static str,
    },
    /// The rendered GPU buffer itself - `encode.rs` imports the fd into a hardware encoder on the
    /// same GPU without any CPU copy.
    Dmabuf(Dmabuf),
}

/// The GPU device the compositor renders on, as a reference-counted fd Smithay can share between
/// the GBM device, EGL display, and allocator.
type RenderDevice = GbmDevice<DrmDeviceFd>;

const TARGET_FPS: u64 = 30;

/// Longest gap between published frames on a still screen. An iPhone leaves up to four seconds.
const IDLE_REFRESH: Duration = Duration::from_secs(1);

/// What the compositor thread reports back once it's up and listening.
struct Ready {
    socket_name: OsString,
    loop_signal: LoopSignal,
}

/// Where the app is drawn inside the stream, in stream pixels.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ViewRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// A request to re-size/re-time the running compositor, sent once the accessory's real display
/// parameters are known. `ack` lets [`Compositor::reconfigure`] block until the mode is live.
struct ReconfigureRequest {
    width: u32,
    height: u32,
    /// Refresh rate in fps; `0` (accessory didn't report one) falls back to the default.
    fps: u32,
    /// The accessory's view: origin and size within the stream. `None` covers the whole of it.
    view: Option<ViewRect>,
    /// Reports back `Ok(true)` if the mode actually changed, `Ok(false)` if it was already at
    /// these parameters, or the failure.
    ack: std::sync::mpsc::Sender<std::result::Result<bool, String>>,
}

/// How many buffers are cycled for whatever gets handed to the encoder. One in flight is not
/// enough - GStreamer holds a pushed dmabuf until the encoder is done with it, so re-rendering
/// into it immediately would tear the frame being encoded.
const FORWARDED_POOL: usize = 4;

/// Handle to a running internal compositor. Dropping it stops the compositor thread - the session
/// flow restarts on failure, so a leaked compositor per restart would pile up.
pub struct Compositor {
    /// Wayland socket name to point desktop client apps at (via `WAYLAND_DISPLAY`).
    pub socket_name: OsString,
    frame_rx: tokio::sync::watch::Receiver<Option<Arc<CapturedFrame>>>,
    touch_tx: calloop::channel::Sender<TouchEvent>,
    control_tx: calloop::channel::Sender<ReconfigureRequest>,
    frame_requested: Arc<AtomicBool>,
    loop_signal: LoopSignal,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Compositor {
    /// Start a headless compositor rendering at `width`x`height` @ `fps` on `render_node`.
    /// `frame_format` must match the resolved encode backend - it picks whether frames are
    /// forwarded as a dmabuf, converted to NV12 on the GPU first, or read back to system memory.
    /// Blocks until the socket is listening; the size/rate are a starting point, refined later by
    /// [`reconfigure`](Self::reconfigure).
    pub fn start(
        render_node: String,
        frame_format: FrameFormat,
        importable_modifiers: Vec<u64>,
        width: u32,
        height: u32,
        fps: u32,
        writeback: ZeroCopyMode,
    ) -> Result<Compositor> {
        let (frame_tx, frame_rx) = tokio::sync::watch::channel(None);
        let (touch_tx, touch_rx) = calloop::channel::channel::<TouchEvent>();
        let (control_tx, control_rx) = calloop::channel::channel::<ReconfigureRequest>();
        // Setup runs on the compositor thread and reports readiness (or a setup error) back here,
        // so `start` can return a ready-to-use handle.
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<Ready>>();
        let frame_requested = Arc::new(AtomicBool::new(false));
        let thread_frame_requested = frame_requested.clone();

        let thread = std::thread::Builder::new()
            .name("carjack-compositor".into())
            .spawn(move || {
                if let Err(e) = run(
                    render_node,
                    frame_format,
                    importable_modifiers,
                    width,
                    height,
                    fps,
                    writeback,
                    frame_tx,
                    touch_rx,
                    control_rx,
                    thread_frame_requested,
                    &ready_tx,
                ) {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .map_err(|e| eyre!("spawn compositor thread: {e}"))?;

        let ready = ready_rx
            .recv()
            .map_err(|_| eyre!("compositor thread exited before signalling ready"))??;

        Ok(Compositor {
            socket_name: ready.socket_name,
            frame_rx,
            touch_tx,
            control_tx,
            frame_requested,
            loop_signal: ready.loop_signal,
            thread: Some(thread),
        })
    }

    pub fn subscribe_frames(&self) -> tokio::sync::watch::Receiver<Option<Arc<CapturedFrame>>> {
        self.frame_rx.clone()
    }

    pub fn send_touch(&self, event: &TouchEvent) -> Result<()> {
        self.touch_tx
            .send(event.clone())
            .map_err(|e| eyre!("send touch to compositor: {e}"))
    }

    /// Ask for a frame even if nothing has changed - what a keyframe request needs, since a
    /// keyframe can only ride out on a frame that gets published.
    pub fn request_frame(&self) {
        self.frame_requested.store(true, Ordering::Relaxed);
    }

    /// A handle for [`Compositor::request_frame`] that can outlive a borrow of the compositor.
    pub fn frame_requester(&self) -> Arc<AtomicBool> {
        self.frame_requested.clone()
    }

    /// Re-size and re-time the running compositor, blocking until the mode is applied. Returns
    /// whether it actually changed (`false` = already at these parameters).
    pub fn reconfigure(
        &self,
        width: u32,
        height: u32,
        fps: u32,
        view: Option<ViewRect>,
    ) -> Result<bool> {
        let (ack_tx, ack_rx) = std::sync::mpsc::channel();
        self.control_tx
            .send(ReconfigureRequest {
                width,
                height,
                fps,
                view,
                ack: ack_tx,
            })
            .map_err(|e| eyre!("send reconfigure to compositor: {e}"))?;
        ack_rx
            .recv()
            .map_err(|_| eyre!("compositor thread exited before reconfigure ack"))?
            .map_err(|e| eyre!("compositor reconfigure failed: {e}"))
    }
}

impl Drop for Compositor {
    fn drop(&mut self) {
        // Tear down the Wayland socket and render loop, so a session restart starts clean.
        self.loop_signal.stop();
        self.loop_signal.wakeup();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Open the DRM render node to render on. Must be the same GPU as the encoder, for zero-copy.
pub(crate) fn open_render_node(path: &str) -> Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| eyre!("open render node {path}: {e}"))
}

/// Bring up a headless GLES renderer on `gbm`'s GPU.
pub(crate) fn make_renderer(gbm: &RenderDevice) -> Result<GlesRenderer> {
    // SAFETY: the EGLDisplay/context are used only on this (the compositor) thread; the context
    // is made current here and never shared across threads.
    let egl_display =
        unsafe { EGLDisplay::new(gbm.clone()) }.map_err(|e| eyre!("create EGLDisplay: {e}"))?;
    let egl_context = EGLContext::new(&egl_display).map_err(|e| eyre!("create EGLContext: {e}"))?;
    let renderer =
        unsafe { GlesRenderer::new(egl_context) }.map_err(|e| eyre!("create GlesRenderer: {e}"))?;
    Ok(renderer)
}

/// The compositor's whole state. Lives on the compositor thread only.
struct State {
    display_handle: DisplayHandle,
    start_time: std::time::Instant,

    width: i32,
    height: i32,
    /// Where the client sits inside the stream, and how big it is. The accessory scales whatever
    /// frame it is sent across its whole panel, so a reserved strip has to be blank rows in a
    /// panel-sized frame; sending a view-sized frame just stretches the app over the strip.
    view_offset: Point<i32, Logical>,
    view_size: Size<i32, Logical>,

    space: Space<Window>,
    popups: PopupManager,
    seat: Seat<Self>,
    output: Output,

    compositor_state: CompositorState,
    xdg_shell_state: XdgShellState,
    shm_state: ShmState,
    dmabuf_state: DmabufState,
    // Held to keep the (xdg-)output/dmabuf globals alive for the session; not otherwise read.
    #[allow(dead_code)]
    output_manager_state: OutputManagerState,
    #[allow(dead_code)]
    dmabuf_global: DmabufGlobal,
    seat_state: SeatState<Self>,
    data_device_state: DataDeviceState,

    renderer: GlesRenderer,
    /// Render targets. Forwarding the rendered dmabuf cycles through a pool, since GStreamer holds
    /// it until consumed; the readback and NV12 modes reuse one buffer, because what leaves the
    /// compositor is a copy made before the next frame starts.
    render_pool: Vec<Dmabuf>,
    pool_idx: usize,
    frame_format: FrameFormat,
    /// The GPU ARGB->NV12 pass and its own pool of NV12 targets. `Some` when `frame_format` is
    /// [`FrameFormat::Nv12`] and `writeback` is not - the two are alternatives, never both.
    nv12: Option<Nv12Converter>,
    /// The same conversion on the display controller instead. Preferred when it is available.
    writeback: Option<WritebackConverter>,
    damage_tracker: OutputDamageTracker,
    /// Set by any client commit; cleared when the frame it produced has been published. The
    /// render timer publishes only when it is set - a still screen costs no frames.
    dirty: bool,
    last_published: std::time::Instant,
    frame_tx: tokio::sync::watch::Sender<Option<Arc<CapturedFrame>>>,
    /// Kept so `reconfigure` can re-allocate the render pool at a new size.
    allocator: GbmAllocator<DrmDeviceFd>,
    modifiers: Vec<smithay::backend::allocator::Modifier>,
    /// Render-timer period, re-read each tick so `reconfigure` can change the framerate live.
    frame_interval: Duration,
    /// How long the last published frame spent in its ARGB->NV12 step, for the cadence stats.
    /// Zero on the paths that have no such step.
    convert_time: Duration,
    /// Publish on every tick rather than only on damage (`CARJACK_FORCE_FRAMES`). For measuring
    /// the conversion paths against each other at full rate on a still screen - a still screen
    /// otherwise publishes about once a second, and both paths look the same at that rate.
    force_frames: bool,
}

impl State {
    /// Which conversion the published frames go through, for the stats line.
    fn path_label(&self) -> &'static str {
        if self.writeback.is_some() {
            "writeback"
        } else if self.nv12.is_some() {
            "gpu-nv12"
        } else if self.frame_format == FrameFormat::Argb {
            "argb"
        } else {
            "shm"
        }
    }

    /// Render the desktop into the current pool buffer and publish it - either as the dmabuf
    /// itself (zero-copy) or read back into system memory (CPU path).
    fn render_and_publish(&mut self) {
        let idx = self.pool_idx;
        let clear = [0.0, 0.0, 0.0, 1.0];
        let mut fb = match self.renderer.bind(&mut self.render_pool[idx]) {
            Ok(fb) => fb,
            Err(e) => {
                tracing::warn!("compositor: bind dmabuf failed: {e}");
                return;
            }
        };
        let render = smithay::desktop::space::render_output::<
            _,
            WaylandSurfaceRenderElement<GlesRenderer>,
            _,
            _,
        >(
            &self.output,
            &mut self.renderer,
            &mut fb,
            1.0,
            0,
            [&self.space],
            &[],
            &mut self.damage_tracker,
            clear,
        );
        if let Err(e) = render {
            tracing::warn!("compositor: render_output failed: {e}");
            return;
        }

        self.convert_time = Duration::ZERO;
        if let Some(wb) = self.writeback.as_mut() {
            drop(fb);
            let convert_start = std::time::Instant::now();
            // Submit the composition before the commit. The plane waits on the buffer's implicit
            // fence (drm_gem_plane_helper_prepare_fb), but GL work still batched in Mesa has no
            // fence yet, so without a flush the display controller could read the previous frame.
            // Cheaper than a finish: the kernel does the waiting, not this thread.
            if let Err(e) = self.renderer.with_context(|gl| unsafe { gl.Flush() }) {
                tracing::warn!("compositor: flush before writeback failed: {e}");
            }
            let converted = wb.convert();
            self.convert_time = convert_start.elapsed();
            match converted {
                Ok(buf) => {
                    let _ = self
                        .frame_tx
                        .send(Some(Arc::new(CapturedFrame::Dmabuf(buf))));
                }
                Err(e) => tracing::warn!("compositor: writeback conversion failed: {e:#}"),
            }
        } else if let Some(nv12) = self.nv12.as_mut() {
            drop(fb);
            // No readback: the frame never leaves the GPU, it just changes format.
            let convert_start = std::time::Instant::now();
            let converted = nv12.convert(&mut self.renderer);
            self.convert_time = convert_start.elapsed();
            match converted {
                Ok(buf) => {
                    let _ = self
                        .frame_tx
                        .send(Some(Arc::new(CapturedFrame::Dmabuf(buf))));
                }
                Err(e) => tracing::warn!("compositor: NV12 conversion failed: {e}"),
            }
        } else if self.frame_format == FrameFormat::Argb {
            drop(fb);
            // The clone shares the fds; cycling the pool avoids overwriting a buffer the encoder
            // is still reading.
            let buf = self.render_pool[idx].clone();
            let _ = self
                .frame_tx
                .send(Some(Arc::new(CapturedFrame::Dmabuf(buf))));
            self.pool_idx = (idx + 1) % self.render_pool.len();
        } else {
            // GL readback rather than a dmabuf mmap, so it works with any modifier.
            let region = Rectangle::<i32, BufferCoord>::from_size((self.width, self.height).into());
            match self
                .renderer
                .copy_framebuffer(&fb, region, Fourcc::Argb8888)
            {
                Ok(mapping) => match self.renderer.map_texture(&mapping) {
                    Ok(bytes) => {
                        let _ = self.frame_tx.send(Some(Arc::new(CapturedFrame::Shm {
                            data: bytes.to_vec(),
                            width: self.width as u32,
                            height: self.height as u32,
                            // Argb8888 native-endian => in-memory bytes B,G,R,X (opaque).
                            gst_format: "BGRx",
                        })));
                    }
                    Err(e) => tracing::warn!("compositor: map_texture failed: {e}"),
                },
                Err(e) => tracing::warn!("compositor: copy_framebuffer failed: {e}"),
            }
            drop(fb);
        }

        self.finish_frame();
    }

    /// A tick with nothing to publish: clients still get their frame callbacks, so one that wants
    /// to animate is never left waiting on us.
    fn idle_tick(&mut self) {
        self.finish_frame();
    }

    /// Tell clients to draw their next frame, then flush - the shared tail of a render tick.
    fn finish_frame(&mut self) {
        let now = self.start_time.elapsed();
        let output = self.output.clone();
        self.space.elements().for_each(|window| {
            window.send_frame(&output, now, Some(Duration::ZERO), |_, _| {
                Some(output.clone())
            })
        });
        self.space.refresh();
        self.popups.cleanup();
        let _ = self.display_handle.flush_clients();
    }

    /// Reallocate the render pool, update the output mode and render-timer period, and reconfigure
    /// mapped clients to the new fullscreen size. Runs on the compositor thread; the error is a
    /// `String` so it can cross the ack channel.
    fn reconfigure(
        &mut self,
        width: u32,
        height: u32,
        fps: u32,
        view: Option<ViewRect>,
    ) -> std::result::Result<bool, String> {
        let fps = if fps == 0 { TARGET_FPS as u32 } else { fps };
        let frame_interval = Duration::from_micros(1_000_000 / fps as u64);
        let view = view.unwrap_or(ViewRect {
            x: 0,
            y: 0,
            width: width as i32,
            height: height as i32,
        });
        let view_offset: Point<i32, Logical> = (view.x, view.y).into();
        let view_size: Size<i32, Logical> = (view.width, view.height).into();
        if self.width == width as i32
            && self.height == height as i32
            && self.frame_interval == frame_interval
            && self.view_offset == view_offset
            && self.view_size == view_size
        {
            return Ok(false); // already at these parameters
        }
        tracing::info!("compositor: reconfiguring to {width}x{height} @ {fps}fps");

        // First, so a failure leaves the old pool and mode untouched.
        let pool_size = self.render_pool.len();
        let mut new_pool = Vec::with_capacity(pool_size);
        for _ in 0..pool_size {
            let buffer = self
                .allocator
                .create_buffer(width, height, Fourcc::Argb8888, &self.modifiers)
                .map_err(|e| format!("allocate render buffer: {e}"))?;
            new_pool.push(buffer.export().map_err(|e| format!("export dmabuf: {e}"))?);
        }
        // The NV12 targets and source texture are sized to the old mode, so the conversion is
        // rebuilt around the new pool - before anything is swapped in, so a failure leaves the
        // compositor rendering at the old size rather than at one it cannot publish.
        //
        // Writeback resizes in place instead of being rebuilt: it holds DRM master, and a
        // replacement could not take it until this one was dropped.
        if let Some(wb) = self.writeback.as_mut() {
            wb.resize(&mut self.allocator, &new_pool[0], width, height)
                .map_err(|e| format!("rebuild writeback conversion: {e:#}"))?;
        }
        let new_nv12 = match self.frame_format {
            FrameFormat::Nv12 if self.writeback.is_none() => Some(
                Nv12Converter::new(
                    &mut self.renderer,
                    &mut self.allocator,
                    &new_pool[0],
                    width,
                    height,
                    FORWARDED_POOL,
                )
                .map_err(|e| format!("rebuild NV12 conversion: {e}"))?,
            ),
            _ => None,
        };
        if let Some(old) = self.nv12.take() {
            old.destroy(&mut self.renderer);
        }
        self.nv12 = new_nv12;
        self.render_pool = new_pool;
        self.pool_idx = 0;
        self.width = width as i32;
        self.height = height as i32;
        self.frame_interval = frame_interval;
        self.view_offset = view_offset;
        self.view_size = view_size;

        let mode = Mode {
            size: (width as i32, height as i32).into(),
            refresh: fps as i32 * 1000,
        };
        self.output.change_current_state(
            Some(mode),
            Some(Transform::Normal),
            None,
            Some((0, 0).into()),
        );
        self.output.set_preferred(mode);
        self.damage_tracker = OutputDamageTracker::from_output(&self.output);

        // Cloned out first: remapping borrows the space that `elements()` is walking.
        let windows: Vec<Window> = self.space.elements().cloned().collect();
        for window in windows {
            if let Some(toplevel) = window.toplevel() {
                toplevel.with_pending_state(|s| {
                    s.size = Some(view_size);
                    s.states.set(xdg_toplevel::State::Fullscreen);
                    s.states.set(xdg_toplevel::State::Activated);
                });
                toplevel.send_configure();
            }
            self.space.map_element(window, view_offset, true);
        }

        // So a subscriber connecting right after this returns - the encoder does - latches its
        // caps onto the new dimensions, not a stale frame still sitting in the watch channel.
        self.render_and_publish();
        Ok(true)
    }

    fn inject_touch(&mut self, event: TouchEvent) {
        let Some(touch) = self.seat.get_touch() else {
            tracing::warn!("inject_touch: seat has no touch handle");
            return;
        };
        let loc = self.touch_location(&event);
        let serial = smithay::utils::SERIAL_COUNTER.next_serial();
        let time = self.start_time.elapsed().as_millis() as u32;
        let slot = Some(event.slot).into();
        match event.event_type {
            TouchEventType::Down => {
                // Clamping alone isn't enough: an edge pixel the app's surface or input region
                // doesn't cover still misses, so down needs the same fallback motion uses.
                let focus = self.touch_focus(loc);
                // Both points: the accessory's bottom edge arrives one row past the surface, and
                // only the clamped one decides whether focus latches.
                tracing::debug!(
                    "inject_touch DOWN slot={} reported=({},{}) clamped=({},{}) output={}x{} focus={}",
                    event.slot,
                    event.x,
                    event.y,
                    loc.x,
                    loc.y,
                    self.width,
                    self.height,
                    focus.is_some()
                );
                touch.down(
                    self,
                    focus,
                    &DownEvent {
                        slot,
                        location: loc,
                        serial,
                        time,
                    },
                );
                touch.frame(self);
            }
            TouchEventType::Motion => {
                let focus = self.touch_focus(loc);
                tracing::trace!(
                    "inject_touch MOTION slot={} reported=({},{}) clamped=({},{})",
                    event.slot,
                    event.x,
                    event.y,
                    loc.x,
                    loc.y
                );
                touch.motion(
                    self,
                    focus,
                    &MotionEvent {
                        slot,
                        location: loc,
                        time,
                    },
                );
                touch.frame(self);
            }
            TouchEventType::Up => {
                tracing::debug!(
                    "inject_touch UP slot={} reported=({},{})",
                    event.slot,
                    event.x,
                    event.y
                );
                touch.up(self, &UpEvent { slot, serial, time });
                touch.frame(self);
            }
        }
    }

    fn surface_under(&self, pos: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        self.space
            .element_under(pos)
            .and_then(|(window, location)| {
                window
                    .surface_under(pos - location.to_f64(), WindowSurfaceType::ALL)
                    .map(|(s, p)| (s, (p + location).to_f64()))
            })
    }

    /// Touch focus, falling back to the top window when the point hits nothing.
    ///
    /// Wayland latches focus at down, so a down that misses drops the whole sequence - and the
    /// points that miss are the screen edges, which is where Android's system gestures live. A
    /// miss here means the app's surface doesn't cover the output to the pixel, or its input
    /// region doesn't; in a single-app kiosk, giving the edge to the app beats losing the gesture.
    fn touch_focus(&self, pos: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        if let Some(focus) = self.surface_under(pos) {
            return Some(focus);
        }
        let window = self.space.elements().last()?;
        let location = self.space.element_location(window)?;
        let surface = window.toplevel()?.wl_surface().clone();
        Some((surface, location.to_f64()))
    }

    /// Accessory coordinates, clamped into the output. A digitizer that reports one row past the
    /// last pixel would otherwise put every bottom-edge swipe outside every surface.
    fn touch_location(&self, event: &TouchEvent) -> Point<f64, Logical> {
        Point::from((
            event.x.clamp(0.0, (self.width - 1).max(0) as f64),
            event.y.clamp(0.0, (self.height - 1).max(0) as f64),
        ))
    }
}

/// `p50/p95/max` in milliseconds, for the cadence stats. Sorts in place.
fn percentiles(samples: &mut [Duration]) -> String {
    if samples.is_empty() {
        return "-".into();
    }
    samples.sort_unstable();
    let at = |q: f64| samples[((samples.len() - 1) as f64 * q).round() as usize];
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    format!(
        "{:.2}/{:.2}/{:.2}ms",
        ms(at(0.5)),
        ms(at(0.95)),
        ms(samples[samples.len() - 1])
    )
}

#[allow(clippy::too_many_arguments)]
fn run(
    render_node: String,
    frame_format: FrameFormat,
    importable_modifiers: Vec<u64>,
    width: u32,
    height: u32,
    fps: u32,
    writeback: ZeroCopyMode,
    frame_tx: tokio::sync::watch::Sender<Option<Arc<CapturedFrame>>>,
    touch_rx: calloop::channel::Channel<TouchEvent>,
    control_rx: calloop::channel::Channel<ReconfigureRequest>,
    frame_requested: Arc<AtomicBool>,
    ready_tx: &std::sync::mpsc::Sender<Result<Ready>>,
) -> Result<()> {
    let fps = fps.max(1);
    let file = open_render_node(&render_node)?;
    let device_fd = DrmDeviceFd::new(smithay::utils::DeviceFd::from(OwnedFd::from(file)));
    let gbm = GbmDevice::new(device_fd).map_err(|e| eyre!("create GbmDevice: {e}"))?;
    let mut renderer = make_renderer(&gbm)?;

    // A modifier the GPU can render *into* - NVIDIA rejects a Linear dmabuf but accepts its own
    // tiled modifiers. Falls back to Linear if the driver advertises none.
    let render_modifiers: Vec<smithay::backend::allocator::Modifier> = renderer
        .egl_context()
        .dmabuf_render_formats()
        .iter()
        .filter(|f| f.code == Fourcc::Argb8888)
        .map(|f| f.modifier)
        .collect();
    tracing::info!(
        "compositor: {} renderable Argb8888 modifier(s) on this GPU",
        render_modifiers.len()
    );
    // An empty intersection keeps the full set: guessing wrong here would break rendering
    // outright, where letting negotiation fail at least says so.
    let render_modifiers = if importable_modifiers.is_empty() {
        render_modifiers
    } else {
        let usable: Vec<_> = render_modifiers
            .iter()
            .copied()
            .filter(|m| importable_modifiers.contains(&u64::from(*m)))
            .collect();
        if usable.is_empty() {
            tracing::warn!(
                "compositor: none of the {} renderable modifier(s) are importable by the encoder; \
                 leaving the choice to GBM",
                render_modifiers.len()
            );
            render_modifiers
        } else {
            tracing::info!(
                "compositor: {} of them importable by the encoder",
                usable.len()
            );
            usable
        }
    };
    let mut allocator = GbmAllocator::new(gbm.clone(), GbmBufferFlags::RENDERING);
    // Owned so `reconfigure` can re-allocate the pool at a new size later.
    let modifiers: Vec<smithay::backend::allocator::Modifier> = if render_modifiers.is_empty() {
        vec![smithay::backend::allocator::Modifier::Linear]
    } else {
        render_modifiers
    };
    // The encoder holds a forwarded dmabuf until consumed, so that mode needs a pool. The other
    // two reuse one buffer: readback is synchronous, and the NV12 pass has finished reading the
    // ARGB buffer by the time it returns - its own targets are the ones cycled.
    let pool_size = if frame_format == FrameFormat::Argb {
        FORWARDED_POOL
    } else {
        1
    };
    let mut render_pool = Vec::with_capacity(pool_size);
    for _ in 0..pool_size {
        let gbm_buffer = allocator
            .create_buffer(width, height, Fourcc::Argb8888, &modifiers)
            .map_err(|e| eyre!("allocate render buffer: {e}"))?;
        render_pool.push(
            gbm_buffer
                .export()
                .map_err(|e| eyre!("export dmabuf: {e}"))?,
        );
    }
    // Two ways to make NV12 out of what was just rendered. The display controller's writeback
    // block is preferred when it is asked for and works; the GPU shader pass is the fallback and
    // the only option everywhere else. Both are set up before the loop starts, so a failure here
    // is a startup warning rather than a dropped frame later.
    let mut wb = None;
    if frame_format == FrameFormat::Nv12 && writeback != ZeroCopyMode::Off {
        match WritebackConverter::new(
            &render_node,
            &mut allocator,
            &render_pool[0],
            width,
            height,
            FORWARDED_POOL,
        ) {
            Ok(converter) => wb = Some(converter),
            Err(e) if writeback == ZeroCopyMode::Force => {
                return Err(e.wrap_err("graphics.writeback is `true` and the path is unavailable"));
            }
            Err(e) => tracing::warn!("compositor: no display-controller NV12 ({e:#}); using the GPU"),
        }
    }
    let nv12 = match frame_format {
        FrameFormat::Nv12 if wb.is_none() => Some(Nv12Converter::new(
            &mut renderer,
            &mut allocator,
            &render_pool[0],
            width,
            height,
            FORWARDED_POOL,
        )?),
        _ => None,
    };
    tracing::info!(
        "compositor: encode path = {}",
        match frame_format {
            FrameFormat::Argb => "zero-copy dmabuf",
            FrameFormat::Nv12 if wb.is_some() => "zero-copy dmabuf (writeback NV12 conversion)",
            FrameFormat::Nv12 => "zero-copy dmabuf (GPU NV12 conversion)",
            FrameFormat::Shm => "CPU readback",
        }
    );

    let mut event_loop: EventLoop<State> =
        EventLoop::try_new().map_err(|e| eyre!("create event loop: {e}"))?;
    let display: Display<State> = Display::new().map_err(|e| eyre!("create display: {e}"))?;
    let dh = display.handle();

    let compositor_state = CompositorState::new::<State>(&dh);
    let xdg_shell_state = XdgShellState::new::<State>(&dh);
    let shm_state = ShmState::new::<State>(&dh, vec![]);
    let output_manager_state = OutputManagerState::new_with_xdg_output::<State>(&dh);
    let mut seat_state = SeatState::<State>::new();
    let data_device_state = DataDeviceState::new::<State>(&dh);
    let popups = PopupManager::default();

    // Without zwp_linux_dmabuf_v1, GPU-accelerated clients have only wl_shm, and Waydroid in
    // particular cannot render at all.
    let mut dmabuf_state = DmabufState::new();
    let dmabuf_formats: Vec<_> = renderer.dmabuf_formats().into_iter().collect();
    let dmabuf_global = dmabuf_state.create_global::<State>(&dh, dmabuf_formats);

    let mut seat = seat_state.new_wl_seat(&dh, "carplay");
    seat.add_keyboard(Default::default(), 200, 25)
        .map_err(|e| eyre!("add keyboard: {e}"))?;
    seat.add_touch();

    // At exactly the accessory's resolution, so nothing downstream has to scale.
    let output = Output::new(
        "carplay".to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "carjack".into(),
            model: "carplay".into(),
        },
    );
    let _output_global = output.create_global::<State>(&dh);
    let mode = Mode {
        size: (width as i32, height as i32).into(),
        refresh: fps as i32 * 1000,
    };
    output.change_current_state(
        Some(mode),
        Some(Transform::Normal),
        None,
        Some((0, 0).into()),
    );
    output.set_preferred(mode);

    let mut space = Space::default();
    space.map_output(&output, (0, 0));
    let damage_tracker = OutputDamageTracker::from_output(&output);

    // Listening socket for desktop client apps.
    let listening_socket =
        ListeningSocketSource::new_auto().map_err(|e| eyre!("create wayland socket: {e}"))?;
    let socket_name = listening_socket.socket_name().to_os_string();

    let loop_handle = event_loop.handle();
    loop_handle
        .insert_source(listening_socket, move |stream, _, state: &mut State| {
            if let Err(e) = state
                .display_handle
                .insert_client(stream, Arc::new(ClientState::default()))
            {
                tracing::warn!("compositor: failed to insert client: {e}");
            }
        })
        .map_err(|e| eyre!("insert socket source: {e}"))?;

    loop_handle
        .insert_source(
            Generic::new(display, Interest::READ, CalloopMode::Level),
            |_, display, state: &mut State| {
                // SAFETY: the display is not dropped for the lifetime of the loop.
                unsafe {
                    display.get_mut().dispatch_clients(state).unwrap();
                }
                Ok(PostAction::Continue)
            },
        )
        .map_err(|e| eyre!("insert display source: {e}"))?;

    loop_handle
        .insert_source(touch_rx, |event, _, state: &mut State| {
            if let calloop::channel::Event::Msg(touch) = event {
                state.inject_touch(touch);
            }
        })
        .map_err(|e| eyre!("insert touch source: {e}"))?;

    // Reconfigure requests (resolution/refresh learned from AirPlay after startup).
    loop_handle
        .insert_source(control_rx, |event, _, state: &mut State| {
            if let calloop::channel::Event::Msg(req) = event {
                let res = state.reconfigure(req.width, req.height, req.fps, req.view);
                if let Err(ref e) = res {
                    tracing::warn!("compositor: reconfigure failed: {e}");
                }
                let _ = req.ack.send(res);
            }
        })
        .map_err(|e| eyre!("insert control source: {e}"))?;

    // A fixed deadline rather than "now + interval", so render time doesn't accumulate into the
    // period and sag the rate under load.
    let frame_interval = Duration::from_micros(1_000_000 / fps as u64);
    let mut next_deadline = std::time::Instant::now() + frame_interval;
    // Cadence stats, logged ~every 5s.
    let mut stat_count: u32 = 0;
    let mut stat_published: u32 = 0;
    let mut stat_render_max = Duration::ZERO;
    let mut stat_since = std::time::Instant::now();
    // Per published frame: the whole tick, and the ARGB->NV12 step within it. The step is timed
    // from the end of composition's submission to the NV12 buffer being ready, so on both paths it
    // includes waiting for composition's GPU work - the GPU pass ends in a finish, and the
    // writeback commit waits on the render target's fence. That makes the two comparable.
    let mut stat_frame: Vec<Duration> = Vec::new();
    let mut stat_convert: Vec<Duration> = Vec::new();
    loop_handle
        .insert_source(
            Timer::from_duration(frame_interval),
            move |_, _, state: &mut State| {
                let render_start = std::time::Instant::now();
                // A frame every IDLE_REFRESH even when nothing moves: the decoder gets a heartbeat,
                // and a keyframe asked for on a still screen has a buffer to ride out on.
                let publish = state.dirty
                    || state.force_frames
                    || frame_requested.swap(false, Ordering::Relaxed)
                    || state.last_published.elapsed() >= IDLE_REFRESH;
                if publish {
                    state.render_and_publish();
                    state.dirty = false;
                    state.last_published = std::time::Instant::now();
                    stat_published += 1;
                } else {
                    state.idle_tick();
                }
                let now = std::time::Instant::now();

                stat_count += 1;
                stat_render_max = stat_render_max.max(now - render_start);
                if publish {
                    stat_frame.push(now - render_start);
                    stat_convert.push(state.convert_time);
                }
                let elapsed = now - stat_since;
                if elapsed >= Duration::from_secs(5) {
                    tracing::info!(
                        "compositor: {}: {:.1} fps published of {:.1} polled (target {}), worst render {:.1}ms, \
                         frame {}, convert {}",
                        state.path_label(),
                        stat_published as f64 / elapsed.as_secs_f64(),
                        stat_count as f64 / elapsed.as_secs_f64(),
                        (1_000_000 / state.frame_interval.as_micros().max(1)),
                        stat_render_max.as_secs_f64() * 1000.0,
                        percentiles(&mut stat_frame),
                        percentiles(&mut stat_convert),
                    );
                    stat_count = 0;
                    stat_published = 0;
                    stat_render_max = Duration::ZERO;
                    stat_frame.clear();
                    stat_convert.clear();
                    stat_since = now;
                }

                next_deadline += state.frame_interval;
                if next_deadline <= now {
                    next_deadline = now + state.frame_interval;
                }
                TimeoutAction::ToInstant(next_deadline)
            },
        )
        .map_err(|e| eyre!("insert render timer: {e}"))?;

    let loop_signal = event_loop.get_signal();
    let mut state = State {
        display_handle: dh,
        start_time: std::time::Instant::now(),
        width: width as i32,
        height: height as i32,
        // No accessory yet, so the app covers the whole stream until one reports its view.
        view_offset: (0, 0).into(),
        view_size: (width as i32, height as i32).into(),
        space,
        popups,
        seat,
        output,
        compositor_state,
        xdg_shell_state,
        shm_state,
        dmabuf_state,
        output_manager_state,
        dmabuf_global,
        seat_state,
        data_device_state,
        renderer,
        render_pool,
        pool_idx: 0,
        frame_format,
        nv12,
        writeback: wb,
        damage_tracker,
        dirty: true,
        last_published: std::time::Instant::now(),
        frame_tx,
        allocator,
        modifiers,
        frame_interval,
        convert_time: Duration::ZERO,
        force_frames: std::env::var_os("CARJACK_FORCE_FRAMES").is_some(),
    };
    if state.force_frames {
        tracing::warn!("compositor: CARJACK_FORCE_FRAMES set - publishing every tick, for measurement");
    }

    tracing::info!(
        socket = ?socket_name,
        width, height,
        "compositor: up and listening"
    );
    ready_tx
        .send(Ok(Ready {
            socket_name,
            loop_signal,
        }))
        .map_err(|_| eyre!("compositor: caller dropped before ready"))?;

    event_loop
        .run(None, &mut state, |_| {})
        .map_err(|e| eyre!("event loop: {e}"))?;
    Ok(())
}

// ---- Wayland protocol handlers ----

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        self.dirty = true;
        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            if let Some(window) = self.space.elements().find(|w| {
                w.toplevel()
                    .map(|t| t.wl_surface() == &root)
                    .unwrap_or(false)
            }) {
                window.on_commit();
            }
        }
        self.popups.commit(surface);
    }
}

impl BufferHandler for State {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        // Force every app fullscreen at the accessory's view - single-app kiosk.
        let size = self.view_size;
        surface.with_pending_state(|state| {
            state.size = Some(size);
            state.states.set(xdg_toplevel::State::Fullscreen);
            state.states.set(xdg_toplevel::State::Activated);
        });
        surface.send_configure();
        let window = Window::new_wayland_window(surface);
        self.space.map_element(window, self.view_offset, true);
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        let _ = self
            .popups
            .track_popup(smithay::desktop::PopupKind::Xdg(surface));
    }

    fn grab(
        &mut self,
        _surface: PopupSurface,
        _seat: smithay::reexports::wayland_server::protocol::wl_seat::WlSeat,
        _serial: smithay::utils::Serial,
    ) {
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        surface.send_repositioned(token);
    }
}

impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn cursor_image(
        &mut self,
        _seat: &Seat<Self>,
        _image: smithay::input::pointer::CursorImageStatus,
    ) {
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let dh = &self.display_handle;
        let client = focused.and_then(|s| {
            use smithay::reexports::wayland_server::Resource;
            dh.get_client(s.id()).ok()
        });
        set_data_device_focus(dh, seat, client);
    }
}

impl SelectionHandler for State {
    type SelectionUserData = ();
}

impl DataDeviceHandler for State {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl ClientDndGrabHandler for State {}
impl ServerDndGrabHandler for State {}

impl OutputHandler for State {}

impl DmabufHandler for State {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        // The render loop re-uses this same import when compositing the client's surface.
        if self.renderer.import_dmabuf(&dmabuf, None).is_ok() {
            let _ = notifier.successful::<State>();
        } else {
            notifier.failed();
        }
    }
}

delegate_compositor!(State);
delegate_shm!(State);
delegate_xdg_shell!(State);
delegate_seat!(State);
delegate_data_device!(State);
delegate_output!(State);
delegate_dmabuf!(State);

#[derive(Default)]
struct ClientState {
    compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}
