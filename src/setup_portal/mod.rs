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

use carjack::config::Wifi;

use crate::wifi_connect::setup_ap::SetupAp;

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
}

impl Portal {
    /// Serve the portal, raising the setup AP first if the uplink adapter has nothing better to
    /// do. Never fatal: a board with one adapter still gets the portal on whatever network it is
    /// already on, and a board with none still runs CarPlay.
    pub async fn start(wifi: Wifi, adapter: Adapter, bluetooth_address: &str) -> Self {
        let ap = if SetupAp::uplink_is_idle(&wifi).await {
            let ssid = ap_ssid();
            let passphrase = ap_passphrase(bluetooth_address);
            match SetupAp::raise(&wifi, &ssid, &passphrase).await {
                Ok(ap) => {
                    tracing::info!(%ssid, %passphrase, "Setup network is up");
                    Some(ap)
                }
                Err(e) => {
                    tracing::warn!("Could not raise the setup network: {e}");
                    None
                }
            }
        } else {
            tracing::info!("Uplink adapter is already on a network; not raising the setup one");
            None
        };

        let state = Arc::new(PortalState {
            wifi,
            adapter,
            ap: tokio::sync::Mutex::new(ap),
        });
        let shutdown = CancellationToken::new();
        let server = tokio::spawn(serve(state, shutdown.clone()));
        Portal {
            shutdown,
            _server: tokio_util::task::AbortOnDropHandle::new(server),
        }
    }

    pub fn stop(&self) {
        self.shutdown.cancel();
    }
}

/// Above 1024 so the user service can bind it without privileges, and matched by the DHCP option
/// 114 URL in `dnsmasq-shared-carjack.conf`. Change one and change the other.
pub const PORT: u16 = 8080;

/// Serve the portal until `shutdown` fires.
///
/// Bound on all interfaces: reachable on the setup AP, and on whatever network the uplink adapter
/// later joins, which is how you reach it once the AP is gone.
pub async fn serve(state: Arc<PortalState>, shutdown: tokio_util::sync::CancellationToken) {
    let listener = match TcpListener::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, PORT))).await {
        Ok(listener) => listener,
        Err(e) => {
            tracing::warn!("Config portal could not bind port {PORT}: {e}");
            return;
        }
    };
    tracing::info!(
        "Config portal on http://{}:{PORT}/",
        crate::wifi_connect::setup_ap::AP_ADDRESS
    );

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

/// A WPA2 passphrase for the setup AP, derived from the adapter's Bluetooth address.
///
/// Stable for a board, so it can go on a label, and printed at startup because a headless board
/// has nowhere else to show it. It stops a passer-by joining and reading the page, and it
/// encrypts the home WiFi passphrase the portal collects - but it is **not** a secret against
/// someone in Bluetooth range, who can see the address and compute this. Deriving it is the
/// trade for being able to recover it without a screen; a random per-board key would be stronger
/// and unknowable on first boot.
pub fn ap_passphrase(bluetooth_address: &str) -> String {
    let digits: String = bluetooth_address
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    format!("carjack-{}", digits.to_lowercase())
}

/// The SSID the setup AP advertises.
pub fn ap_ssid() -> String {
    String::from("carjack-setup")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `view!` macros are checked at compile time, but whether the router actually serves them
    /// - layout, route table, status - only shows up at runtime. `/` is the one page that reads
    /// no hardware, so it is the one that can be asked for without a head unit or an adapter.
    #[tokio::test]
    async fn the_status_page_renders() {
        let router = Router::builder().discover().build();
        let request = topcoat::router::request::Request::builder()
            .uri("/")
            .body(topcoat::router::Body::empty())
            .expect("building a request");

        let response = router.handle(request).await;
        assert_eq!(response.status(), 200);

        let body = topcoat::router::to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("reading the body");
        let html = String::from_utf8_lossy(&body);

        // The shell layout wrapped it, and the page itself rendered inside.
        assert!(html.contains("<title>carjack setup</title>"), "{html}");
        assert!(html.contains("href=\"/unit\""), "{html}");
        assert!(html.contains("href=\"/wifi\""), "{html}");
        assert!(html.contains("Head unit"), "{html}");
    }

    #[test]
    fn the_ap_passphrase_is_long_enough_for_wpa2_and_stable() {
        let passphrase = ap_passphrase("AA:BB:CC:DD:EE:FF");
        assert!(
            passphrase.len() >= 8,
            "WPA2 needs 8 characters: {passphrase}"
        );
        assert_eq!(passphrase, ap_passphrase("aa:bb:cc:dd:ee:ff"));
        assert_ne!(passphrase, ap_passphrase("11:22:33:44:55:66"));
    }
}
