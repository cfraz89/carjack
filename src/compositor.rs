//! Internal Wayland compositor (Smithay-based). carjack runs its own minimal headless compositor
//! so it can render at exactly the car's resolution, render on the same GPU it encodes on
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
use std::time::Duration;

use eyre::{Result, eyre};
use libcarjack::carplay_session::{TouchEvent, TouchEventType};
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
use smithay::utils::{Buffer as BufferCoord, Logical, Point, Rectangle, Transform};
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
    /// The rendered GPU buffer itself, forwarded zero-copy - `encode.rs` imports the fd into a
    /// hardware encoder on the same GPU (glupload -> nvh264enc) without any CPU copy.
    Dmabuf(Dmabuf),
}

/// The GPU device the compositor renders on, as a reference-counted fd Smithay can share
/// between the GBM device, EGL display, and allocator.
type RenderDevice = GbmDevice<DrmDeviceFd>;

const TARGET_FPS: u64 = 30;

/// What the compositor thread reports back once it's up and listening.
struct Ready {
    socket_name: OsString,
    loop_signal: LoopSignal,
}

/// A request to re-size/re-time the running compositor. Sent from the session thread once the
/// car's real display parameters are known; `ack` reports success/failure back to the caller so
/// [`Compositor::reconfigure`] can block until the new mode is live.
struct ReconfigureRequest {
    width: u32,
    height: u32,
    /// Refresh rate in fps; `0` (car didn't report one) falls back to the default.
    fps: u32,
    /// Reports back `Ok(true)` if the mode actually changed, `Ok(false)` if it was already at
    /// these parameters, or the failure.
    ack: std::sync::mpsc::Sender<std::result::Result<bool, String>>,
}

/// Handle to a running internal compositor. Owns the frame stream and the touch input channel;
/// the compositor itself runs on its own thread. Dropping it stops that thread (important: the
/// session flow restarts on failure, so a leaked compositor per restart would pile up).
pub struct Compositor {
    /// Wayland socket name to point desktop client apps at (via `WAYLAND_DISPLAY`).
    pub socket_name: OsString,
    frame_rx: tokio::sync::watch::Receiver<Option<Arc<CapturedFrame>>>,
    touch_tx: calloop::channel::Sender<TouchEvent>,
    control_tx: calloop::channel::Sender<ReconfigureRequest>,
    loop_signal: LoopSignal,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Compositor {
    /// Start a headless compositor rendering at `width`x`height` @ `fps` on `render_node`.
    /// `zero_copy` must match the resolved encode backend - it picks whether frames are forwarded
    /// as a dmabuf or read back to system memory. Blocks until the socket is listening; the
    /// size/rate are a starting point, refined later by [`reconfigure`](Self::reconfigure).
    pub fn start(
        render_node: String,
        zero_copy: bool,
        width: u32,
        height: u32,
        fps: u32,
    ) -> Result<Compositor> {
        let (frame_tx, frame_rx) = tokio::sync::watch::channel(None);
        let (touch_tx, touch_rx) = calloop::channel::channel::<TouchEvent>();
        let (control_tx, control_rx) = calloop::channel::channel::<ReconfigureRequest>();
        // Setup runs on the compositor thread; it reports readiness (or a setup error) back
        // here so `start` can return a ready-to-use handle.
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<Ready>>();

        let thread = std::thread::Builder::new()
            .name("carjack-compositor".into())
            .spawn(move || {
                if let Err(e) = run(
                    render_node,
                    zero_copy,
                    width,
                    height,
                    fps,
                    frame_tx,
                    touch_rx,
                    control_rx,
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

    /// Re-size and re-time the running compositor to the car's real display parameters. Blocks
    /// until the compositor thread has applied the new mode (or reports the failure). Returns
    /// whether the mode actually changed (`false` = it was already at these parameters).
    pub fn reconfigure(&self, width: u32, height: u32, fps: u32) -> Result<bool> {
        let (ack_tx, ack_rx) = std::sync::mpsc::channel();
        self.control_tx
            .send(ReconfigureRequest {
                width,
                height,
                fps,
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
        // Stop the event loop and wake it so it exits promptly, then join the thread - this
        // tears down the Wayland socket and render loop so a session restart starts clean.
        self.loop_signal.stop();
        self.loop_signal.wakeup();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Open the DRM render node to render on. Must be the same GPU as the encoder (the zero-copy
/// requirement); the concrete node is resolved from config in `main` and passed in.
fn open_render_node(path: &str) -> Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| eyre!("open render node {path}: {e}"))
}

/// Bring up a headless GLES renderer on `gbm`'s GPU.
fn make_renderer(gbm: &RenderDevice) -> Result<GlesRenderer> {
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
    /// Render targets. Zero-copy mode cycles through a small pool (a forwarded dmabuf is held
    /// by GStreamer until it's consumed, so we can't re-render into it immediately); the CPU
    /// readback mode uses a single reused buffer.
    render_pool: Vec<Dmabuf>,
    pool_idx: usize,
    zero_copy: bool,
    damage_tracker: OutputDamageTracker,
    frame_tx: tokio::sync::watch::Sender<Option<Arc<CapturedFrame>>>,
    /// Kept so `reconfigure` can re-allocate the render pool at a new size (with the same
    /// modifiers chosen for this GPU at startup).
    allocator: GbmAllocator<DrmDeviceFd>,
    modifiers: Vec<smithay::backend::allocator::Modifier>,
    /// Current render-timer period (derived from the car's refresh rate); read by the timer each
    /// tick so `reconfigure` can change the framerate live.
    frame_interval: Duration,
}

impl State {
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

        if self.zero_copy {
            drop(fb);
            // Forward the rendered GPU buffer itself - no readback, no CPU copy. The clone
            // shares the fds; cycling the pool keeps us from overwriting a buffer the encoder
            // is still reading.
            let buf = self.render_pool[idx].clone();
            let _ = self
                .frame_tx
                .send(Some(Arc::new(CapturedFrame::Dmabuf(buf))));
            self.pool_idx = (idx + 1) % self.render_pool.len();
        } else {
            // Read the framebuffer back into system memory (GPU->CPU copy) - what the software
            // encode path needs. Works with any modifier (GL readback, not a dmabuf mmap).
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

        // Tell clients to draw their next frame, then flush.
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

    /// Re-size/re-time the compositor to the car's real display parameters: reallocates the render
    /// pool, updates the output mode and render-timer period, and reconfigures mapped clients to
    /// the new fullscreen size. Runs on the compositor thread; the error is a `String` so it can
    /// cross the ack channel.
    fn reconfigure(
        &mut self,
        width: u32,
        height: u32,
        fps: u32,
    ) -> std::result::Result<bool, String> {
        let fps = if fps == 0 { TARGET_FPS as u32 } else { fps };
        let frame_interval = Duration::from_micros(1_000_000 / fps as u64);
        if self.width == width as i32
            && self.height == height as i32
            && self.frame_interval == frame_interval
        {
            return Ok(false); // already at these parameters
        }
        tracing::info!("compositor: reconfiguring to {width}x{height} @ {fps}fps");

        // Reallocate the render pool at the new size (same count + modifiers as at startup). Do
        // this first so a failure leaves the old pool/mode untouched.
        let pool_size = self.render_pool.len();
        let mut new_pool = Vec::with_capacity(pool_size);
        for _ in 0..pool_size {
            let buffer = self
                .allocator
                .create_buffer(width, height, Fourcc::Argb8888, &self.modifiers)
                .map_err(|e| format!("allocate render buffer: {e}"))?;
            new_pool.push(buffer.export().map_err(|e| format!("export dmabuf: {e}"))?);
        }
        self.render_pool = new_pool;
        self.pool_idx = 0;
        self.width = width as i32;
        self.height = height as i32;
        self.frame_interval = frame_interval;

        // Update the output mode (size + refresh) and rebuild the size-specific damage tracker.
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

        // Reconfigure every mapped client to the new fullscreen size.
        let size = (self.width, self.height);
        for window in self.space.elements() {
            if let Some(toplevel) = window.toplevel() {
                toplevel.with_pending_state(|s| {
                    s.size = Some(size.into());
                    s.states.set(xdg_toplevel::State::Fullscreen);
                    s.states.set(xdg_toplevel::State::Activated);
                });
                toplevel.send_configure();
            }
        }

        // Publish a frame at the new size immediately so a subscriber that connects right after
        // this returns (the encoder does) latches its caps onto the new dimensions, not a stale
        // pre-resize frame still sitting in the watch channel.
        self.render_and_publish();
        Ok(true)
    }

    fn inject_touch(&mut self, event: TouchEvent) {
        let Some(touch) = self.seat.get_touch() else {
            tracing::warn!("inject_touch: seat has no touch handle");
            return;
        };
        let loc = Point::<f64, Logical>::from((event.x, event.y));
        let serial = smithay::utils::SERIAL_COUNTER.next_serial();
        let time = self.start_time.elapsed().as_millis() as u32;
        let slot = Some(event.slot).into();
        match event.event_type {
            TouchEventType::Down => {
                let focus = self.surface_under(loc);
                tracing::debug!(
                    "inject_touch DOWN slot={} ({},{}) focus={}",
                    event.slot,
                    event.x,
                    event.y,
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
                let focus = self.surface_under(loc);
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
}

#[allow(clippy::too_many_arguments)]
fn run(
    render_node: String,
    zero_copy: bool,
    width: u32,
    height: u32,
    fps: u32,
    frame_tx: tokio::sync::watch::Sender<Option<Arc<CapturedFrame>>>,
    touch_rx: calloop::channel::Channel<TouchEvent>,
    control_rx: calloop::channel::Channel<ReconfigureRequest>,
    ready_tx: &std::sync::mpsc::Sender<Result<Ready>>,
) -> Result<()> {
    let fps = fps.max(1);
    let file = open_render_node(&render_node)?;
    let device_fd = DrmDeviceFd::new(smithay::utils::DeviceFd::from(OwnedFd::from(file)));
    let gbm = GbmDevice::new(device_fd).map_err(|e| eyre!("create GbmDevice: {e}"))?;
    let renderer = make_renderer(&gbm)?;

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
    let mut allocator = GbmAllocator::new(gbm.clone(), GbmBufferFlags::RENDERING);
    // Kept (owned) so `reconfigure` can re-allocate the pool at a new size later.
    let modifiers: Vec<smithay::backend::allocator::Modifier> = if render_modifiers.is_empty() {
        vec![smithay::backend::allocator::Modifier::Linear]
    } else {
        render_modifiers
    };
    // Zero-copy mode forwards each rendered dmabuf to the encoder, which holds it until
    // consumed - so we cycle through a small pool. The CPU-readback path reuses a single buffer
    // (readback is synchronous, no aliasing).
    let pool_size = if zero_copy { 4 } else { 1 };
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
    tracing::info!(
        "compositor: encode path = {}",
        if zero_copy {
            "zero-copy dmabuf"
        } else {
            "CPU readback"
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

    // Advertise zwp_linux_dmabuf_v1 with the renderer's supported formats so GPU-accelerated
    // clients (Waydroid, browsers, games) can share their rendered frames as dmabufs - without
    // this they only have wl_shm and Waydroid in particular can't render at all.
    let mut dmabuf_state = DmabufState::new();
    let dmabuf_formats: Vec<_> = renderer.dmabuf_formats().into_iter().collect();
    let dmabuf_global = dmabuf_state.create_global::<State>(&dh, dmabuf_formats);

    let mut seat = seat_state.new_wl_seat(&dh, "carplay");
    seat.add_keyboard(Default::default(), 200, 25)
        .map_err(|e| eyre!("add keyboard: {e}"))?;
    seat.add_touch();

    // Headless output at exactly the car's resolution, so clients render fullscreen at the
    // right size and no scaling is needed downstream.
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
                let res = state.reconfigure(req.width, req.height, req.fps);
                if let Err(ref e) = res {
                    tracing::warn!("compositor: reconfigure failed: {e}");
                }
                let _ = req.ack.send(res);
            }
        })
        .map_err(|e| eyre!("insert control source: {e}"))?;

    // Rearm to a fixed deadline rather than "now + interval", so render time doesn't accumulate
    // into the period and sag the rate under load. Re-read from `state.frame_interval` each tick
    // so `reconfigure` can change it live.
    let frame_interval = Duration::from_micros(1_000_000 / fps as u64);
    let mut next_deadline = std::time::Instant::now() + frame_interval;
    // Cadence stats, logged ~every 5s.
    let mut stat_count: u32 = 0;
    let mut stat_render_max = Duration::ZERO;
    let mut stat_since = std::time::Instant::now();
    loop_handle
        .insert_source(
            Timer::from_duration(frame_interval),
            move |_, _, state: &mut State| {
                let render_start = std::time::Instant::now();
                state.render_and_publish();
                let now = std::time::Instant::now();

                stat_count += 1;
                stat_render_max = stat_render_max.max(now - render_start);
                let elapsed = now - stat_since;
                if elapsed >= Duration::from_secs(5) {
                    tracing::info!(
                        "compositor: {:.1} fps rendered (target {}), worst render {:.1}ms",
                        stat_count as f64 / elapsed.as_secs_f64(),
                        (1_000_000 / state.frame_interval.as_micros().max(1)),
                        stat_render_max.as_secs_f64() * 1000.0,
                    );
                    stat_count = 0;
                    stat_render_max = Duration::ZERO;
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
        zero_copy,
        damage_tracker,
        frame_tx,
        allocator,
        modifiers,
        frame_interval,
    };

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
        // Force every app fullscreen at the car's resolution - single-app kiosk.
        let size = (self.width, self.height);
        surface.with_pending_state(|state| {
            state.size = Some(size.into());
            state.states.set(xdg_toplevel::State::Fullscreen);
            state.states.set(xdg_toplevel::State::Activated);
        });
        surface.send_configure();
        let window = Window::new_wayland_window(surface);
        self.space.map_element(window, (0, 0), true);
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
        // Validate by importing into our renderer; the render loop re-uses this same import
        // when compositing the client's surface.
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
