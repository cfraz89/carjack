use std::net::SocketAddr;
use std::time::{Duration, Instant};

use catplay_carplay::ctrl::CarPlayCtrlServer;
use catplay_util::EventSleeper;
use eyre::{Result, eyre};
use uuid::Uuid;

/// Advertise `_carplay-ctrl._tcp` via mDNS, wait for the car's `ctrl-int/1/connect` invite,
/// and correlate it against the car's own `_airplay._tcp` Bonjour record to learn its real
/// AirPlay address and HomeKit pairing id (rather than guessing the WiFi gateway IP and
/// skipping HomeKit pair-verify fast-path).
pub async fn wait_for_carplay_ctrl_probe(iface: &str, bonjour_id: &str) -> Result<(SocketAddr, Option<Uuid>)> {
    const PORT: u16 = 8082;

    let mut ctrl_server = CarPlayCtrlServer::new(bonjour_id, PORT, iface)
        .map_err(|e| eyre!("Failed to start CarPlay ctrl server: {e}"))?;
    tracing::info!("_carplay-ctrl._tcp advertised on {iface}:{PORT} (id={bonjour_id})");

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut announce_interval = tokio::time::interval(Duration::from_millis(1000));

    loop {
        if let Some(invite) = ctrl_server.pop_invite() {
            let addr = invite
                .entry
                .meta
                .addrs
                .first()
                .copied()
                .ok_or_else(|| eyre!("CarPlay invite had no AirPlay address in Bonjour record"))?;
            tracing::info!(
                "Got CarPlay ctrl invite; AirPlay peer at {addr}, pi={:?}",
                invite.entry.data.pi
            );
            return Ok((addr, invite.entry.data.pi));
        }

        if Instant::now() >= deadline {
            return Err(eyre!("Timeout waiting for CarPlay ctrl invite"));
        }

        // ctrl_server.sleep() isn't optional here: pop_invite()'s underlying channel is a
        // peek-cache that only gets filled by actually polling this EventSleeper - calling
        // pop_invite() on its own (without ever awaiting sleep()) silently never sees
        // invites that already arrived over TCP.
        tokio::select! {
            _ = announce_interval.tick() => {
                let _ = ctrl_server.force_announce();
            }
            _ = ctrl_server.sleep() => {}
        }
    }
}
