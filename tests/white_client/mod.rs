//! A minimal Wayland client for the compositor tests: maps one toplevel and paints it white, so
//! the tests need nothing installed to have something to composite.

use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eyre::{Result, eyre};
use wayland_client::protocol::{
    wl_buffer, wl_callback, wl_compositor, wl_registry, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

const FILL: u32 = 0x00ff_ffff;

/// A running client. Dropping it stops the thread and disconnects.
pub struct WhiteClient {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for WhiteClient {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Connect to `socket_name` in `$XDG_RUNTIME_DIR` and run until the handle is dropped. By path
/// rather than `WAYLAND_DISPLAY`: the test process may have its own compositor in that variable.
pub fn spawn(socket_name: &std::ffi::OsStr) -> Result<WhiteClient> {
    let runtime = std::env::var("XDG_RUNTIME_DIR")
        .map_err(|_| eyre!("XDG_RUNTIME_DIR is not set; cannot find the compositor socket"))?;
    let path = std::path::Path::new(&runtime).join(socket_name);
    let stream =
        UnixStream::connect(&path).map_err(|e| eyre!("connect to {}: {e}", path.display()))?;
    let conn = Connection::from_socket(stream).map_err(|e| eyre!("wayland connection: {e}"))?;

    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
    let thread = std::thread::Builder::new()
        .name("carjack-test-client".into())
        .spawn(move || {
            if let Err(e) = run(conn, &thread_stop, &ready_tx) {
                let _ = ready_tx.send(Err(e));
            }
        })
        .map_err(|e| eyre!("spawn client thread: {e}"))?;

    ready_rx
        .recv()
        .map_err(|_| eyre!("client thread exited before signalling ready"))??;
    Ok(WhiteClient {
        stop,
        thread: Some(thread),
    })
}

fn run(
    conn: Connection,
    stop: &AtomicBool,
    ready_tx: &std::sync::mpsc::Sender<Result<()>>,
) -> Result<()> {
    let mut queue = conn.new_event_queue::<State>();
    let qh = queue.handle();
    let _registry = conn.display().get_registry(&qh, ());
    let mut state = State::default();
    queue
        .roundtrip(&mut state)
        .map_err(|e| eyre!("registry roundtrip: {e}"))?;

    let (compositor, shm, wm_base) = match (&state.compositor, &state.shm, &state.wm_base) {
        (Some(c), Some(s), Some(w)) => (c.clone(), s.clone(), w.clone()),
        _ => {
            return Err(eyre!(
                "compositor is missing a global the client needs (wl_compositor={}, wl_shm={}, \
                 xdg_wm_base={})",
                state.compositor.is_some(),
                state.shm.is_some(),
                state.wm_base.is_some()
            ));
        }
    };

    let surface = compositor.create_surface(&qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_title("carjack test client".into());
    surface.commit();
    state.surface = Some(surface.clone());
    state.shm = Some(shm);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !state.painted {
        if std::time::Instant::now() > deadline {
            return Err(eyre!(
                "compositor never configured the test client's surface"
            ));
        }
        queue
            .blocking_dispatch(&mut state)
            .map_err(|e| eyre!("dispatch while mapping: {e}"))?;
    }
    let _ = ready_tx.send(Ok(()));

    // Once the test drops the compositor the socket closes and dispatch errors - the normal end.
    while !stop.load(Ordering::Relaxed) && !state.closed {
        if queue.blocking_dispatch(&mut state).is_err() {
            break;
        }
    }
    Ok(())
}

#[derive(Default)]
struct State {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    wm_base: Option<xdg_wm_base::XdgWmBase>,
    surface: Option<wl_surface::WlSurface>,
    size: (i32, i32),
    buffer: Option<wl_buffer::WlBuffer>,
    painted: bool,
    closed: bool,
}

impl State {
    /// Attach and commit, asking for a frame callback. The compositor publishes a frame only when
    /// a client has committed, so painting once would leave the frame stream idle after one frame.
    fn paint(&mut self, qh: &QueueHandle<Self>) {
        let (Some(surface), Some(shm)) = (self.surface.clone(), self.shm.clone()) else {
            return;
        };
        let (width, height) = self.size;
        if width <= 0 || height <= 0 {
            return;
        }
        if self.buffer.is_none() {
            match solid_buffer(&shm, qh, width, height) {
                Ok(buffer) => self.buffer = Some(buffer),
                Err(e) => {
                    eprintln!("test client: {e}");
                    return;
                }
            }
        }
        surface.attach(self.buffer.as_ref(), 0, 0);
        surface.damage_buffer(0, 0, width, height);
        surface.frame(qh, ());
        surface.commit();
        self.painted = true;
    }
}

fn solid_buffer(
    shm: &wl_shm::WlShm,
    qh: &QueueHandle<State>,
    width: i32,
    height: i32,
) -> Result<wl_buffer::WlBuffer> {
    let stride = width * 4;
    let len = (stride * height) as usize;
    let fd = unsafe { libc::memfd_create(c"carjack-test-client".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(eyre!("memfd_create: {}", std::io::Error::last_os_error()));
    }
    let file = unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(fd) };
    file.set_len(len as u64)
        .map_err(|e| eyre!("size shm buffer: {e}"))?;
    let map = unsafe {
        let p = libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            0,
        );
        if p == libc::MAP_FAILED {
            return Err(eyre!(
                "mmap shm buffer: {}",
                std::io::Error::last_os_error()
            ));
        }
        std::slice::from_raw_parts_mut(p as *mut u32, len / 4)
    };
    map.fill(FILL);
    unsafe { libc::munmap(map.as_mut_ptr() as *mut _, len) };

    let pool = shm.create_pool(file.as_fd(), len as i32, qh, ());
    let buffer = pool.create_buffer(0, width, height, stride, wl_shm::Format::Xrgb8888, qh, ());
    pool.destroy();
    Ok(buffer)
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                // v4+ for `damage_buffer`.
                "wl_compositor" => {
                    state.compositor = registry
                        .bind::<wl_compositor::WlCompositor, (), Self>(name, version.min(4), qh, ())
                        .into();
                }
                "wl_shm" => {
                    state.shm = registry
                        .bind::<wl_shm::WlShm, (), Self>(name, version.min(1), qh, ())
                        .into();
                }
                "xdg_wm_base" => {
                    state.wm_base = registry
                        .bind::<xdg_wm_base::XdgWmBase, (), Self>(name, version.min(1), qh, ())
                        .into();
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for State {
    fn event(
        _: &mut Self,
        wm_base: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for State {
    fn event(
        state: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            xdg_toplevel::Event::Configure { width, height, .. } => {
                if (width, height) != state.size && width > 0 && height > 0 {
                    state.size = (width, height);
                    state.buffer = None;
                }
            }
            xdg_toplevel::Event::Close => state.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for State {
    fn event(
        state: &mut Self,
        xdg_surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            state.paint(qh);
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            state.paint(qh);
        }
    }
}

delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_buffer::WlBuffer);
delegate_noop!(State: ignore wl_surface::WlSurface);
delegate_noop!(State: ignore wl_compositor::WlCompositor);
