//! The headless config portal: a small Topcoat app carjack serves while it has nothing to connect
//! to, so a phone on the setup AP can pick a head unit and give the board a WiFi network.
//!
//! Deliberately plain forms. The first contact is usually through iOS's captive-portal sheet,
//! which is a stripped browser and a poor place for anything clever; the same pages are also what
//! you get typing the address into a real browser later, which is how every visit after the first
//! actually happens.

mod pages;
mod scan;

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use bluer::Adapter;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use topcoat::router::{Router, RouterBuilderDiscoverExt};

use carjack::config::{Config, Wifi};
use carjack::stored_settings::StoredSettings;

use crate::wifi_connect::setup_ap::{SetupAp, UplinkState};

/// Shared with every page through Topcoat's app context.
pub struct PortalState {
    pub wifi: Wifi,
    pub adapter: Adapter,
    /// The setup AP, while it is up. Joining a network on the uplink adapter has to take this
    /// down first - they are the same radio.
    pub ap: tokio::sync::Mutex<Option<SetupAp>>,
}

/// The portal, running for as long as this is held.
pub struct Portal {
    shutdown: CancellationToken,
    _server: tokio_util::task::AbortOnDropHandle<()>,
    _uplink: tokio_util::task::AbortOnDropHandle<()>,
}

impl Portal {
    /// Serve the portal, and raise the setup AP alongside it if the uplink adapter has nothing
    /// better to do. Never fatal: a board with one adapter still gets the portal on whatever
    /// network it is already on, and a board with none still runs CarPlay.
    ///
    /// The AP decision is spawned, not awaited: settling it waits on NetworkManager, and the
    /// CarPlay session starts behind this call.
    ///
    /// `None` when `[portal] enabled` is false, and then nothing is raised or served.
    pub async fn start(config: &Config, adapter: Adapter) -> Option<Self> {
        if !config.portal.enabled {
            tracing::info!("Config portal disabled by [portal] enabled = false");
            return None;
        }
        let state = Arc::new(PortalState {
            wifi: config.wifi.clone(),
            adapter,
            ap: tokio::sync::Mutex::new(None),
        });

        let uplink = tokio::spawn({
            let state = state.clone();
            async move {
                match SetupAp::uplink_state(&state.wifi).await {
                    UplinkState::Idle => *state.ap.lock().await = raise_ap(&state.wifi).await,
                    UplinkState::Busy => tracing::info!(
                        "Uplink adapter is already on a network; not raising the setup one"
                    ),
                    UplinkState::Absent => tracing::info!(
                        "No adapter holds the uplink role, so there is no radio free for the \
                         setup network; serving the portal on whatever this board is already on"
                    ),
                }
            }
        });

        let shutdown = CancellationToken::new();
        let server = tokio::spawn(serve(state, shutdown.clone()));
        Some(Portal {
            shutdown,
            _server: tokio_util::task::AbortOnDropHandle::new(server),
            _uplink: tokio_util::task::AbortOnDropHandle::new(uplink),
        })
    }

    pub fn stop(&self) {
        self.shutdown.cancel();
    }
}

/// Raise the setup AP on the stored passphrase, warning loudly while it is still the shipped one.
pub async fn raise_ap(wifi: &Wifi) -> Option<SetupAp> {
    let settings = StoredSettings::load();
    let ssid = ap_ssid();
    let passphrase = settings.ap_passphrase().to_owned();
    match SetupAp::raise(wifi, &ssid, &passphrase).await {
        Ok(ap) => {
            if settings.ap_passphrase_is_default() {
                tracing::warn!(
                    %ssid, %passphrase,
                    "Setup network is up on the password it shipped with - the portal will ask \
                     for a new one before it does anything else"
                );
            } else {
                tracing::info!(%ssid, "Setup network is up");
            }
            Some(ap)
        }
        Err(e) => {
            tracing::warn!("Could not raise the setup network: {e}");
            None
        }
    }
}

/// What the packaged DHCP option 114 URL names, and what the packaged sysctl drop-in
/// (`carjack-sysctl.conf`) lowers `ip_unprivileged_port_start` to so a user service can bind it.
pub const PORT: u16 = 80;

/// Used when port 80 is refused, which is what happens without that sysctl - a dev box, or a
/// package installed without it.
pub const FALLBACK_PORT: u16 = 8080;

/// Serve the portal until `shutdown` fires.
///
/// Bound on all interfaces: reachable on the setup AP, and on whatever network the uplink adapter
/// later joins, which is how you reach it once the AP is gone.
pub async fn serve(state: Arc<PortalState>, shutdown: tokio_util::sync::CancellationToken) {
    let address = crate::wifi_connect::setup_ap::AP_ADDRESS;
    let listener = match TcpListener::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, PORT))).await {
        Ok(listener) => {
            tracing::info!("Config portal on http://{address}/");
            listener
        }
        Err(e) => {
            match TcpListener::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, FALLBACK_PORT))).await
            {
                Ok(listener) => {
                    // The option 114 URL is baked into a packaged file and still names port 80,
                    // so on this path phones will not be sent anywhere useful.
                    tracing::warn!(
                        "Port {PORT} refused ({e}); serving the config portal on \
                         http://{address}:{FALLBACK_PORT}/ instead. Phones will not find it on \
                         their own - install carjack's sysctl drop-in to get port {PORT}"
                    );
                    listener
                }
                Err(fallback) => {
                    tracing::warn!(
                        "Config portal could not bind port {PORT} ({e}) or {FALLBACK_PORT} \
                         ({fallback})"
                    );
                    return;
                }
            }
        }
    };

    let router = Router::builder().discover().app_context(state).build();
    tokio::select! {
        _ = shutdown.cancelled() => tracing::info!("Config portal stopped"),
        result = topcoat::serve(listener, router) => {
            if let Err(e) = result {
                tracing::warn!("Config portal stopped: {e}");
            }
        }
    }
}

/// The SSID the setup AP advertises.
pub fn ap_ssid() -> String {
    String::from("carjack-setup")
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn get(router: &topcoat::router::Router, path: &str) -> (u16, String) {
        let request = topcoat::router::request::Request::builder()
            .uri(path)
            .body(topcoat::router::Body::empty())
            .expect("building a request");
        let response = router.handle(request).await;
        let status = response.status().as_u16();
        let body = topcoat::router::to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("reading the body");
        (status, String::from_utf8_lossy(&body).into_owned())
    }

    /// The `view!` macros are checked at compile time, but whether the router actually serves them
    /// - layout, route table, the password gate - only shows up at runtime. These are the pages
    /// that read no hardware, so they are the ones reachable without a head unit or an adapter.
    ///
    /// One test rather than three: they share a `STATE_DIRECTORY`, and `cargo test` would run
    /// separate ones concurrently on top of each other.
    #[tokio::test]
    async fn the_shipped_password_gates_every_page_until_it_is_changed() {
        let state_dir =
            std::env::temp_dir().join(format!("carjack-portal-test-{}", std::process::id()));
        std::fs::create_dir_all(&state_dir).expect("making a state dir");
        // SAFETY: the only test in this binary that reads STATE_DIRECTORY, and it does not spawn
        // threads that would race on the environment.
        unsafe { std::env::set_var("STATE_DIRECTORY", &state_dir) };

        let router = Router::builder().discover().build();

        // Fresh board: the AP is on the password it shipped with, so nothing else is reachable.
        assert!(StoredSettings::load().ap_passphrase_is_default());
        let (status, _) = get(&router, "/").await;
        assert_eq!(status, 307, "/ should redirect to the password page");

        let (status, html) = get(&router, "/password").await;
        assert_eq!(status, 200);
        assert!(html.contains("<title>carjack setup</title>"), "{html}");
        assert!(html.contains("name=\"password\""), "{html}");
        assert!(
            html.contains("still on the password carjack ships with"),
            "{html}"
        );

        StoredSettings::set_ap_passphrase("a better secret").expect("saving a password");

        // Changed: the rest of the portal opens up.
        let (status, html) = get(&router, "/").await;
        assert_eq!(status, 200);
        assert!(html.contains("href=\"/unit\""), "{html}");
        assert!(html.contains("href=\"/wifi\""), "{html}");
        assert!(html.contains("Head unit"), "{html}");

        std::fs::remove_dir_all(&state_dir).ok();
    }

    /// The packaged option 114 URL and sysctl drop-in both name this one.
    #[test]
    fn the_advertised_port_is_the_privileged_one() {
        assert_eq!(PORT, 80);
        assert_ne!(FALLBACK_PORT, PORT);
    }
}
