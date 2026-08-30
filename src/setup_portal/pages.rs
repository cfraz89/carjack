//! The portal's pages. Plain forms, one page per thing you can change.

use std::sync::Arc;

use serde::Deserialize;
use topcoat::{
    Result,
    context::{Cx, app_context},
    router::{content::Form, layout, page},
    view::view,
};

use carjack::config::Target;
use carjack::stored_settings::StoredSettings;

use super::PortalState;
use super::scan;

/// Inlined rather than served as an asset: the page has to render on a network with no route off
/// the board, and a missing stylesheet is a config screen nobody can read.
const STYLE: &str = "\
:root { color-scheme: light dark; }
body { font: 17px/1.5 system-ui, sans-serif; margin: 0 auto; max-width: 34rem; padding: 1.5rem; }
h1 { font-size: 1.4rem; } h2 { font-size: 1.1rem; margin-top: 2rem; }
nav a { margin-right: 1rem; }
label { display: block; padding: .6rem 0; border-bottom: 1px solid #8883; }
input[type=text], input[type=password] { font: inherit; width: 100%; padding: .5rem;
  box-sizing: border-box; margin-top: .4rem; }
button { font: inherit; padding: .7rem 1.2rem; margin-top: 1.2rem; width: 100%; }
.muted { color: #8a8a8a; font-size: .85em; }
.ok { padding: .8rem; border: 1px solid currentColor; margin: 1rem 0; }
";

#[layout("/")]
async fn shell(slot: Result) -> Result {
    view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta name="viewport" content="width=device-width, initial-scale=1">
                <title>"carjack setup"</title>
                <style>(STYLE)</style>
            </head>
            <body>
                <h1>"carjack setup"</h1>
                <nav>
                    <a href="/">"Status"</a>
                    <a href="/unit">"Head unit"</a>
                    <a href="/wifi">"WiFi"</a>
                </nav>
                (slot?)
            </body>
        </html>
    }
}

#[page("/")]
async fn index() -> Result {
    let configured = StoredSettings::load()
        .bluetooth
        .map(|bluetooth| bluetooth.device)
        .unwrap_or_default();

    view! {
        <h2>"Head unit"</h2>
        if configured.is_empty() {
            <p>"Not set. carjack has nothing to connect to."</p>
        } else if configured == "auto" {
            <p>"Set to " <strong>"auto"</strong> " - whichever CarPlay unit is in range."</p>
        } else {
            <p>"Set to " <strong>(configured)</strong> "."</p>
        }
        <p><a href="/unit">"Change the head unit"</a></p>

        <h2>"WiFi"</h2>
        <p class="muted">
            "The second adapter carries the internet connection. Joining a network here also makes \
             this page reachable from it, so you can come back without the setup network."
        </p>
        <p><a href="/wifi">"Choose a network"</a></p>
    }
}

#[derive(Deserialize)]
struct UnitChoice {
    device: String,
}

#[page("/unit")]
async fn unit(cx: &Cx) -> Result {
    let state: &Arc<PortalState> = app_context(cx);
    let units = scan::head_units(&state.adapter).await.unwrap_or_else(|e| {
        tracing::warn!(?e, "Head unit scan failed");
        Vec::new()
    });
    let current = StoredSettings::load()
        .bluetooth
        .map(|bluetooth| bluetooth.device)
        .unwrap_or_default();

    view! {
        <h2>"Head unit"</h2>
        <form method="post" action="/unit">
            <label>
                <input type="radio" name="device" value="auto" checked=(current == "auto" || current.is_empty())>
                <strong>" Automatic"</strong>
                <div class="muted">
                    "Connect to whichever CarPlay unit is in range. Picks the strongest, so it \
                     takes the one in this car."
                </div>
            </label>
            for found in &units {
                <label>
                    <input type="radio" name="device" value=(&found.address) checked=(current == found.address)>
                    " " (&found.name)
                    <div class="muted">(&found.address) " - signal " (found.rssi) " dBm"</div>
                </label>
            }
            if units.is_empty() {
                <p class="muted">
                    "No CarPlay head units answered. Automatic will keep looking every time \
                     carjack starts a session, so it is the safe choice here."
                </p>
            }
            <button type="submit">"Save"</button>
        </form>
    }
}

#[page(POST "/unit")]
async fn save_unit(Form(choice): Form<UnitChoice>) -> Result {
    // Anything that is not a MAC we recognise is stored verbatim; `Target` treats it as a name.
    let saved = Target::parse(&choice.device);
    let message = match &saved {
        None => String::from("Nothing selected."),
        Some(target) => match StoredSettings::set_bluetooth_device(&target.to_string()) {
            Ok(()) => format!("Head unit set to {target}."),
            Err(e) => format!("Could not save: {e}"),
        },
    };

    view! {
        <div class="ok">(message)</div>
        <p>"carjack picks this up on its next connection attempt."</p>
        <p><a href="/">"Back to status"</a></p>
    }
}

#[derive(Deserialize)]
struct NetworkChoice {
    ssid: String,
    #[serde(default)]
    passphrase: String,
}

#[page("/wifi")]
async fn wifi(cx: &Cx) -> Result {
    let state: &Arc<PortalState> = app_context(cx);
    let found = scan::networks(&state.wifi).await;

    view! {
        <h2>"WiFi"</h2>
        match &found {
            Err(e) => {
                <p class="muted">"Could not scan: " (e.to_string())</p>
                <p class="muted">
                    "This needs a second WiFi adapter - the first one is reserved for CarPlay."
                </p>
            }
            Ok(networks) => {
                <form method="post" action="/wifi">
                    for network in networks {
                        <label>
                            <input type="radio" name="ssid" value=(&network.ssid)>
                            " " (&network.ssid)
                            <span class="muted">
                                " - " (network.strength) "%"
                                if !network.secured { " - open" }
                            </span>
                        </label>
                    }
                    if networks.is_empty() {
                        <p class="muted">"No networks found."</p>
                    }
                    <label>
                        "Password"
                        <input type="password" name="passphrase" autocomplete="off">
                    </label>
                    <button type="submit">"Join"</button>
                </form>
            }
        }
    }
}

#[page(POST "/wifi")]
async fn join_wifi(cx: &Cx, Form(choice): Form<NetworkChoice>) -> Result {
    let state: &Arc<PortalState> = app_context(cx);
    // The setup AP and the network being joined are the same radio, so the AP has to go first -
    // NetworkManager would otherwise refuse to activate a second profile on the device.
    if let Some(ap) = state.ap.lock().await.take() {
        ap.lower().await;
    }
    let joined = scan::join_network(&state.wifi, &choice.ssid, &choice.passphrase).await;

    view! {
        match &joined {
            Ok(()) => {
                <div class="ok">"Joining " (&choice.ssid) "."</div>
                <p>
                    "If this network is on the same adapter as the setup network, this page is \
                     about to disappear - that means it worked. Reconnect your phone to your \
                     normal network to check."
                </p>
            }
            Err(e) => {
                <div class="ok">"Could not join: " (e.to_string())</div>
                <p><a href="/wifi">"Try again"</a></p>
            }
        }
        <p><a href="/">"Back to status"</a></p>
    }
}
