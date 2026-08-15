//! Touch injection over **libei** (the EI emulated-input protocol) using the `reis` crate.
//!
//! Input travels over the EI socket from `RemoteDesktop.ConnectToEIS`, not the portal's
//! `NotifyTouch*` D-Bus methods - those validate coordinates against the stream's *advertised*
//! size while Mutter's transform expects *buffer* space, so on a scaled monitor no coordinate
//! satisfies both. An `ei_device`'s regions are already in logical pixels, with no such mismatch.
//! `ConnectToEIS` is portal API v2 (Mutter and KWin both implement it), not GNOME-specific.
//!
//! Threading: `reis`'s event stream is `!Send`, so it runs on its own thread with a current-thread
//! runtime. The async side only sends [`Command`]s down a channel; dropping [`EiTouch`] closes it,
//! ending the thread.
use std::time::Instant;

use ashpd::desktop::{
    Session,
    remote_desktop::{ConnectToEISOptions, RemoteDesktop},
};
use eyre::{Result, eyre};
use futures::StreamExt;
use libcarjack::carplay_session::{TouchEvent, TouchEventType};
use reis::event::{DeviceCapability, EiEvent};
use reis::{ei, event};
use tokio::sync::{mpsc, oneshot};
use tracing::debug;

/// How long to wait for the EIS implementation to offer *and* resume a touchscreen before giving up
/// (and letting the caller fall back to the D-Bus path).
const READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Queue depth for touch commands. Touch arrives at human rates, so this only ever backs up if the
/// EI thread has stalled - at which point dropping the newest events (and saying so) beats growing
/// latency without bound.
const COMMAND_QUEUE: usize = 256;

/// A libei connection with a touchscreen device, ready to take CarPlay touches.
pub struct EiTouch {
    commands: mpsc::Sender<Command>,
}

/// Work for the EI thread.
enum Command {
    /// One touch to inject, in the accessory's coordinate space (mapped into a region on the EI thread,
    /// which is what owns the region list).
    Touch {
        event: TouchEvent,
        accessory_size: (u32, u32),
    },
    /// Lift every finger that's still down - sent when a CarPlay session ends, so an accessory that
    /// disappears mid-gesture doesn't leave the desktop with a finger held down forever.
    ReleaseAll,
}

impl EiTouch {
    /// Ask the portal for an EI socket (`ConnectToEIS`), hand it to a dedicated thread, and wait for
    /// a touchscreen device to be offered and resumed there.
    ///
    /// Fails if the portal is too old for `ConnectToEIS` (API v2), if the compositor has no EIS
    /// implementation, or if no touchscreen appears - each of which is the caller's cue to fall back
    /// to the `NotifyTouch*` D-Bus path.
    pub async fn connect(
        remote_desktop: &RemoteDesktop,
        session: &Session<RemoteDesktop>,
        mapping_id: Option<String>,
    ) -> Result<Self> {
        let fd = remote_desktop
            .connect_to_eis(session, ConnectToEISOptions::default())
            .await
            .map_err(|e| eyre!("ConnectToEIS failed (portal too old for libei?): {e}"))?;
        Self::from_stream(std::os::unix::net::UnixStream::from(fd), mapping_id).await
    }

    /// Drive libei over an already-open EI socket. Split out from [`Self::connect`] so the socket's
    /// provenance (the portal here, but equally `LIBEI_SOCKET` or a test server) is the caller's
    /// business.
    pub async fn from_stream(
        stream: std::os::unix::net::UnixStream,
        mapping_id: Option<String>,
    ) -> Result<Self> {
        let (commands, command_rx) = mpsc::channel(COMMAND_QUEUE);
        let (ready_tx, ready_rx) = oneshot::channel();
        std::thread::Builder::new()
            .name("carjack-libei".into())
            .spawn(move || ei_thread(stream, mapping_id, command_rx, ready_tx))
            .map_err(|e| eyre!("spawn libei thread: {e}"))?;

        match ready_rx.await {
            Ok(Ok(())) => Ok(Self { commands }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(eyre!("libei thread exited during setup")),
        }
    }

    /// Queue a CarPlay touch for injection. `accessory_size` is the accessory's display size (the space
    /// `event.x/.y` are in); the EI thread maps it into the device region matching the streamed
    /// monitor.
    ///
    /// Non-blocking, so it never stalls the session loop; an error means the libei connection is
    /// gone for good.
    pub fn send_touch(&self, event: &TouchEvent, accessory_size: (u32, u32)) -> Result<()> {
        self.send(Command::Touch {
            event: event.clone(),
            accessory_size,
        })
    }

    /// Lift every finger still held down. Call this when a CarPlay session ends: the EI connection
    /// outlives individual sessions, so an accessory that vanishes mid-gesture would otherwise leave the
    /// desktop with a finger stuck down.
    pub fn release_all(&self) -> Result<()> {
        self.send(Command::ReleaseAll)
    }

    fn send(&self, command: Command) -> Result<()> {
        match self.commands.try_send(command) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!("libei command queue full, dropping event");
                Ok(())
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Err(eyre!("libei connection closed")),
        }
    }

    /// Whether the EI thread is still running (it exits on disconnect or protocol error). Used by
    /// the live test to prove an injected touch didn't make the compositor hang up on us.
    #[cfg(test)]
    pub fn is_connected(&self) -> bool {
        !self.commands.is_closed()
    }
}

/// The EI thread: own the `!Send` protocol objects, service the event stream and inject queued
/// touches, for as long as the caller holds the command sender.
fn ei_thread(
    stream: std::os::unix::net::UnixStream,
    mapping_id: Option<String>,
    commands: mpsc::Receiver<Command>,
    ready: oneshot::Sender<Result<()>>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            let _ = ready.send(Err(eyre!("libei runtime: {e}")));
            return;
        }
    };
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, async move {
        if let Err(e) = ei_loop(stream, mapping_id, commands, ready).await {
            tracing::warn!("libei thread ended: {e}");
        }
    });
}

/// Handshake, then service EI events and queued touches until the connection or the queue closes.
async fn ei_loop(
    stream: std::os::unix::net::UnixStream,
    mapping_id: Option<String>,
    mut commands: mpsc::Receiver<Command>,
    ready: oneshot::Sender<Result<()>>,
) -> Result<()> {
    let context = ei::Context::new(stream).map_err(|e| eyre!("EI context: {e}"))?;
    let (connection, mut events) = match context
        .handshake_tokio("carjack", ei::handshake::ContextType::Sender)
        .await
    {
        Ok(pair) => pair,
        Err(e) => {
            let _ = ready.send(Err(eyre!("EI handshake: {e}")));
            return Ok(());
        }
    };

    let mut session = EiSession {
        connection,
        mapping_id,
        started: Instant::now(),
        touch: None,
        sequence: 0,
        ready: Some(ready),
        slots: SlotTracker::default(),
    };
    let deadline = tokio::time::sleep(READY_TIMEOUT);
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            // Devices are offered asynchronously after the handshake; if none can take touch, tell
            // the caller so it can fall back rather than silently swallowing every touch.
            () = &mut deadline, if session.ready.is_some() => {
                session.fail_ready(eyre!(
                    "no libei touchscreen became available within {READY_TIMEOUT:?} (does the \
                     portal session include the touchscreen device type?)"
                ));
                return Ok(());
            }
            event = events.next() => {
                match event {
                    Some(Ok(event)) => session.handle_event(event),
                    Some(Err(e)) => return Err(eyre!("EI event stream: {e}")),
                    None => {
                        session.fail_ready(eyre!("EI server closed the connection"));
                        return Ok(());
                    }
                }
                let _ = session.connection.flush();
            }
            command = commands.recv() => {
                let Some(command) = command else {
                    debug!("libei command channel closed, shutting the EI thread down");
                    return Ok(());
                };
                match &command {
                    Command::Touch { event, accessory_size } => session.inject(event, *accessory_size),
                    Command::ReleaseAll => session.release_all(),
                }
            }
        }
    }
}

/// EI state for one connection: the touchscreen device (once offered and resumed) and what's needed
/// to emit events on it.
struct EiSession {
    connection: event::Connection,
    /// `mapping_id` of the video stream, to pick the region the accessory is actually looking at when the
    /// server offers several (multi-monitor).
    mapping_id: Option<String>,
    /// EI event timestamps are microseconds on an arbitrary but monotonic clock.
    started: Instant,
    touch: Option<TouchDevice>,
    /// `ei_device.start_emulating` takes a sequence number that increases per emulation run.
    sequence: u32,
    /// Answered once, when a touchscreen is ready or it's clear none will be.
    ready: Option<oneshot::Sender<Result<()>>>,
    /// Which touch slots are currently down, so no event can reach the compositor out of order.
    slots: SlotTracker,
}

struct TouchDevice {
    device: event::Device,
    touchscreen: ei::Touchscreen,
    /// `true` between `resumed` and `paused`: only then may events be emitted.
    emulating: bool,
}

impl EiSession {
    /// Track the server's seat/device lifecycle: bind the touch capability on every seat, remember
    /// the touchscreen we're offered, and follow its paused/resumed state.
    fn handle_event(&mut self, event: EiEvent) {
        match event {
            EiEvent::SeatAdded(seat) => {
                // Touch only: the accessory sends nothing else into the desktop.
                seat.seat.bind_capabilities(DeviceCapability::Touch.into());
                debug!("EI seat {:?}: bound touch capability", seat.seat.name());
            }
            EiEvent::DeviceAdded(added) => {
                let device = added.device;
                if !device.has_capability(DeviceCapability::Touch) {
                    debug!("EI device {:?} can't do touch, ignoring", device.name());
                    return;
                }
                let Some(touchscreen) = device.interface::<ei::Touchscreen>() else {
                    tracing::warn!("EI device claims touch but exposes no ei_touchscreen");
                    return;
                };
                debug!(
                    "EI touchscreen added: {:?}, regions {:?}",
                    device.name(),
                    describe_regions(device.regions())
                );
                self.touch = Some(TouchDevice {
                    device,
                    touchscreen,
                    emulating: false,
                });
            }
            EiEvent::DeviceResumed(resumed) => {
                let serial = self.connection.serial();
                let sequence = self.sequence;
                let Some(touch) = self.touch.as_mut().filter(|t| t.device == resumed.device) else {
                    return;
                };
                // start_emulating is mandatory after a resume, before any event is accepted.
                touch.device.device().start_emulating(serial, sequence);
                touch.emulating = true;
                // A fresh emulation run: the compositor holds no slots for us.
                self.slots.clear();
                self.sequence = sequence.wrapping_add(1);
                let regions = describe_regions(touch.device.regions());
                debug!("EI touchscreen resumed (sequence {sequence})");
                if let Some(ready) = self.ready.take() {
                    tracing::info!(
                        "libei touch ready: regions {regions:?}, stream mapping_id {:?}",
                        self.mapping_id
                    );
                    let _ = ready.send(Ok(()));
                }
            }
            EiEvent::DevicePaused(paused) => {
                if let Some(touch) = self.touch.as_mut().filter(|t| t.device == paused.device) {
                    touch.emulating = false;
                    // Mutter releases our slots on pause; forget them so a later `up` from the accessory
                    // is dropped instead of aborting the compositor.
                    self.slots.clear();
                    debug!("EI touchscreen paused");
                }
            }
            EiEvent::DeviceRemoved(removed)
                if self
                    .touch
                    .as_ref()
                    .is_some_and(|t| t.device == removed.device) =>
            {
                self.touch = None;
                self.slots.clear();
                debug!("EI touchscreen removed");
            }
            EiEvent::Disconnected(disconnected) => {
                tracing::warn!(
                    "EI server disconnected: {}",
                    disconnected
                        .explanation
                        .as_deref()
                        .unwrap_or("no reason given")
                );
                self.touch = None;
                self.slots.clear();
                self.fail_ready(eyre!("EI server disconnected"));
            }
            _ => {}
        }
    }

    /// Map a queued touch into the device's region and emit it, if the slot's state allows it.
    ///
    /// Coordinates go into the region in logical pixels, offset by its position - no display-scale
    /// division, since that's what halved touch on the old D-Bus path.
    ///
    /// Every event is filtered through [`SlotTracker`] first: Mutter's EIS treats an `up` for a
    /// slot it never acquired as a fatal `g_assert`, aborting gnome-shell (the whole session), and
    /// a `motion` for an unknown slot silently moves the wrong finger. The accessory can produce either
    /// after a dropped event or a paused device, so nothing reaches the wire unfiltered.
    fn inject(&mut self, event: &TouchEvent, accessory_size: (u32, u32)) {
        let Some(touch) = self.touch.as_ref().filter(|t| t.emulating) else {
            // Paused, or gone: dropping is correct, not an error. The tracker is cleared on pause,
            // so a later `up` for this gesture is dropped too rather than crashing the compositor.
            debug!("libei touch dropped: no device emulating");
            return;
        };
        if !self.slots.allows(event.event_type, event.slot) {
            debug!(
                "libei touch {:?} slot={} dropped: not valid for the slot's current state",
                event.event_type, event.slot
            );
            return;
        }
        let Some(region) = pick_region(touch.device.regions(), self.mapping_id.as_deref()) else {
            debug!("libei touch dropped: touchscreen has no region");
            return;
        };
        let (x, y) = region_position(region, event.x, event.y, accessory_size);
        debug!(
            "libei touch {:?} slot={} accessory=({:.0},{:.0}) accessory_size={accessory_size:?} \
             region={}x{}+{}+{} -> ({x:.1},{y:.1})",
            event.event_type,
            event.slot,
            event.x,
            event.y,
            region.width,
            region.height,
            region.x,
            region.y
        );

        match event.event_type {
            TouchEventType::Down => touch.touchscreen.down(event.slot, x, y),
            TouchEventType::Motion => touch.touchscreen.motion(event.slot, x, y),
            TouchEventType::Up => touch.touchscreen.up(event.slot),
        }
        self.slots.record(event.event_type, event.slot);
        self.frame();
    }

    /// Lift every slot still down (end of a CarPlay session), so nothing stays held on the desktop.
    fn release_all(&mut self) {
        let Some(touch) = self.touch.as_ref().filter(|t| t.emulating) else {
            self.slots.clear();
            return;
        };
        for slot in self.slots.take_down() {
            debug!("libei releasing held slot {slot} at end of session");
            touch.touchscreen.up(slot);
            self.frame();
        }
    }

    /// Close the current event batch. One frame per event: `down` may not share a frame with
    /// `motion` or `up`.
    fn frame(&self) {
        let Some(touch) = self.touch.as_ref() else {
            return;
        };
        touch.device.device().frame(
            self.connection.serial(),
            self.started.elapsed().as_micros() as u64,
        );
        if let Err(e) = self.connection.flush() {
            tracing::warn!("EI flush failed: {e}");
        }
    }

    /// Report a setup failure to `connect`, if it's still waiting.
    fn fail_ready(&mut self, error: eyre::Report) {
        if let Some(ready) = self.ready.take() {
            let _ = ready.send(Err(error));
        }
    }
}

/// Which touch slots are currently down, so carjack only ever emits a well-formed sequence
/// (`down` -> `motion`* -> `up`) per slot - Mutter's EIS aborts the whole compositor on an `up`
/// for a slot it never acquired, so anything malformed the accessory sends has to be filtered here.
#[derive(Default)]
struct SlotTracker {
    down: std::collections::BTreeSet<u32>,
}

impl SlotTracker {
    /// Whether this event may be emitted for `slot` given what the compositor currently believes.
    fn allows(&self, event_type: TouchEventType, slot: u32) -> bool {
        let is_down = self.down.contains(&slot);
        match event_type {
            // A second down for a live slot leaks slots in Mutter until acquire_slot fails.
            TouchEventType::Down => !is_down,
            // Motion/up for a slot that isn't down is the dangerous case (wrong finger, or abort).
            TouchEventType::Motion | TouchEventType::Up => is_down,
        }
    }

    /// Record an event that was emitted.
    fn record(&mut self, event_type: TouchEventType, slot: u32) {
        match event_type {
            TouchEventType::Down => {
                self.down.insert(slot);
            }
            TouchEventType::Up => {
                self.down.remove(&slot);
            }
            TouchEventType::Motion => {}
        }
    }

    /// Forget every slot without emitting anything - for when the compositor has already dropped
    /// them itself (device paused, removed, or the connection went away).
    fn clear(&mut self) {
        self.down.clear();
    }

    /// The slots still down, removing them from the tracker (the caller emits their `up`).
    fn take_down(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.down).into_iter().collect()
    }
}

/// The region to address: the one whose `mapping_id` matches the video stream (that's what the id is
/// for - correlating a region with the stream the client shows), else the only/first one.
fn pick_region<'a>(
    regions: &'a [event::Region],
    mapping_id: Option<&str>,
) -> Option<&'a event::Region> {
    if let Some(id) = mapping_id
        && let Some(region) = regions.iter().find(|r| r.mapping_id.as_deref() == Some(id))
    {
        return Some(region);
    }
    regions.first()
}

/// Map an accessory-space touch into `region`: scale by the region's size, offset by its position, and keep
/// the result inside it - out-of-region coordinates are *silently discarded* by the EIS
/// implementation, and an accessory reports touch right at the edge of its display.
fn region_position(
    region: &event::Region,
    x: f64,
    y: f64,
    accessory_size: (u32, u32),
) -> (f32, f32) {
    let (accessory_w, accessory_h) = (f64::from(accessory_size.0), f64::from(accessory_size.1));
    let (rw, rh) = (f64::from(region.width), f64::from(region.height));
    let (sx, sy) = if accessory_w > 0.0 && accessory_h > 0.0 {
        (x * rw / accessory_w, y * rh / accessory_h)
    } else {
        (x, y)
    };
    // `width`/`height` are extents, so the last addressable pixel is one less.
    let clamp = |v: f64, extent: f64| v.clamp(0.0, (extent - 1.0).max(0.0));
    (
        (f64::from(region.x) + clamp(sx, rw)) as f32,
        (f64::from(region.y) + clamp(sy, rh)) as f32,
    )
}

/// Region geometry as short strings, for logging (`event::Region` is neither `Clone` nor `Debug`).
fn describe_regions(regions: &[event::Region]) -> Vec<String> {
    regions
        .iter()
        .map(|r| {
            format!(
                "{}x{}+{}+{}@{}{}",
                r.width,
                r.height,
                r.x,
                r.y,
                r.scale,
                r.mapping_id
                    .as_deref()
                    .map(|m| format!(" id={m}"))
                    .unwrap_or_default()
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(x: u32, y: u32, width: u32, height: u32, mapping_id: Option<&str>) -> event::Region {
        event::Region {
            x,
            y,
            width,
            height,
            scale: 1.0,
            mapping_id: mapping_id.map(str::to_string),
        }
    }

    #[test]
    fn accessory_touch_maps_into_the_region() {
        // 1080x600 accessory -> a 1728x1080 logical region at the origin.
        let r = region(0, 0, 1728, 1080, None);
        assert_eq!(region_position(&r, 0.0, 0.0, (1080, 600)), (0.0, 0.0));
        assert_eq!(
            region_position(&r, 540.0, 300.0, (1080, 600)),
            (864.0, 540.0)
        );
        // An edge touch stays inside the region rather than being discarded.
        assert_eq!(
            region_position(&r, 1080.0, 600.0, (1080, 600)),
            (1727.0, 1079.0)
        );
        // Overshoot and negatives are pulled in too.
        assert_eq!(
            region_position(&r, -5.0, 9000.0, (1080, 600)),
            (0.0, 1079.0)
        );
    }

    /// A region that isn't at the origin (a second monitor) must have its offset added, so touch
    /// lands on that monitor rather than the primary one.
    #[test]
    fn region_offset_is_applied() {
        let r = region(1728, 0, 1920, 1080, None);
        assert_eq!(
            region_position(&r, 540.0, 300.0, (1080, 600)),
            (1728.0 + 960.0, 540.0)
        );
    }

    /// The safety net that matters most: Mutter *aborts the whole session* on an `up` for a slot it
    /// never acquired, so no malformed sequence from the accessory may ever reach the wire.
    #[test]
    fn slot_tracker_only_allows_well_formed_sequences() {
        use TouchEventType::{Down, Motion, Up};
        let mut slots = SlotTracker::default();

        // Nothing is down: motion and up must be refused (up is the compositor-killing one).
        assert!(!slots.allows(Up, 0));
        assert!(!slots.allows(Motion, 0));
        assert!(slots.allows(Down, 0));

        slots.record(Down, 0);
        assert!(slots.allows(Motion, 0));
        assert!(slots.allows(Up, 0));
        // A second down for a live slot leaks slots in Mutter until acquire_slot fails.
        assert!(!slots.allows(Down, 0));
        // Other slots are independent (multitouch).
        assert!(slots.allows(Down, 1));
        assert!(!slots.allows(Up, 1));

        slots.record(Up, 0);
        assert!(
            !slots.allows(Up, 0),
            "a duplicate up is exactly what crashed gnome-shell"
        );
        assert!(!slots.allows(Motion, 0));
        assert!(
            slots.allows(Down, 0),
            "the slot can be re-used after the up"
        );
    }

    /// A paused/removed device means the compositor dropped its slots; ours must go too, or the
    /// gesture's later `up` becomes a release of an unacquired slot.
    #[test]
    fn clearing_forgets_held_slots() {
        use TouchEventType::{Down, Up};
        let mut slots = SlotTracker::default();
        slots.record(Down, 0);
        slots.record(Down, 1);
        slots.clear();
        assert!(!slots.allows(Up, 0));
        assert!(!slots.allows(Up, 1));

        // take_down hands back the held slots (so their `up` can be emitted) and empties the set.
        slots.record(Down, 2);
        slots.record(Down, 5);
        assert_eq!(slots.take_down(), vec![2, 5]);
        assert!(slots.take_down().is_empty());
        assert!(!slots.allows(Up, 2));
    }

    #[test]
    fn region_is_picked_by_stream_mapping_id() {
        let regions = [
            region(0, 0, 1728, 1080, Some("monitor-a")),
            region(1728, 0, 1920, 1080, Some("monitor-b")),
        ];
        assert_eq!(
            pick_region(&regions, Some("monitor-b")).unwrap().x,
            1728,
            "the region matching the streamed monitor must win"
        );
        // No id, or an id the server doesn't know: fall back to the first region.
        assert_eq!(pick_region(&regions, None).unwrap().x, 0);
        assert_eq!(pick_region(&regions, Some("nope")).unwrap().x, 0);
        assert!(pick_region(&[], Some("monitor-a")).is_none());
    }
}
