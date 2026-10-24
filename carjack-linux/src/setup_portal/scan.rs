//! What the portal's pages have to ask the hardware: which head units are in range, which WiFi
//! networks the uplink adapter can see, and joining one of them.

use std::collections::HashSet;
use std::time::Duration;

use bluer::{Adapter, DiscoveryFilter};
use eyre::{Result, eyre};
use nm_automata::{
    ConnectionProfile, ConnectionSetting, Ipv4Method, Ipv4Setting, Ipv6Method, Ipv6Setting,
    NM80211ApSecurityFlags, ProxyExt, ScanOptions, WirelessProxy, WirelessSecurityKeyMgmt,
    WirelessSecuritySetting, WirelessSetting,
};
use tokio_stream::StreamExt;

use carjack::config::{Wifi, WifiRole};
use libcarjack::uuid::IAP_ACCESSORY_UUID;

use crate::wifi_connect::networkmanager::{get_network_manager, role_device};

/// How long a portal scan runs. Shorter than the session's, because someone is watching the page.
const SCAN_WINDOW: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadUnit {
    pub address: String,
    pub name: String,
    pub rssi: i16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Network {
    pub ssid: String,
    pub strength: u8,
    pub secured: bool,
}

/// Head units in range, strongest first.
///
/// Same rules as the session's `auto` discovery: filtered to the iAP accessory service every
/// CarPlay unit hosts, and gated on RSSI, which BlueZ leaves unset for a device it knows but is
/// not currently seeing.
pub async fn head_units(adapter: &Adapter) -> Result<Vec<HeadUnit>> {
    adapter
        .set_discovery_filter(DiscoveryFilter {
            uuids: HashSet::from([IAP_ACCESSORY_UUID]),
            transport: bluer::DiscoveryTransport::BrEdr,
            rssi: Some(-100),
            ..DiscoveryFilter::default()
        })
        .await?;
    let mut events = adapter.discover_devices_with_changes().await?;

    let deadline = tokio::time::Instant::now() + SCAN_WINDOW;
    loop {
        tokio::select! {
            event = events.next() => {
                if event.is_none() {
                    break;
                }
            }
            _ = tokio::time::sleep_until(deadline) => break,
        }
    }

    let mut found = Vec::new();
    for address in adapter.device_addresses().await? {
        let Ok(device) = adapter.device(address) else {
            continue;
        };
        let Ok(Some(rssi)) = device.rssi().await else {
            continue;
        };
        let offers_iap = device
            .uuids()
            .await
            .ok()
            .flatten()
            .is_some_and(|uuids| uuids.contains(&IAP_ACCESSORY_UUID));
        if !offers_iap {
            continue;
        }
        let name = device
            .alias()
            .await
            .ok()
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| address.to_string());
        found.push(HeadUnit {
            address: address.to_string(),
            name,
            rssi,
        });
    }
    found.sort_by_key(|unit| std::cmp::Reverse(unit.rssi));
    Ok(found)
}

/// Networks the uplink adapter can see, strongest first, one entry per SSID.
pub async fn networks(wifi: &Wifi) -> Result<Vec<Network>> {
    let device = role_device(WifiRole::Uplink, wifi.selector(WifiRole::Uplink)).await?;
    let wireless: WirelessProxy = device.cast().await?;

    // A scan the adapter refuses (it is busy, or one just ran) still leaves the previous results
    // worth showing, so this is not fatal.
    if let Err(e) = wireless.request_scan(&ScanOptions::default()).await {
        tracing::debug!(?e, "Scan request refused; showing the last results");
    } else {
        tokio::time::sleep(SCAN_WINDOW).await;
    }

    let mut best: Vec<Network> = Vec::new();
    for ap in wireless.get_all_access_points().await? {
        let Ok(ssid) = ap.ssid().await else { continue };
        let ssid = ssid.to_string();
        if ssid.is_empty() {
            continue;
        }
        let strength = ap.strength().await.unwrap_or(0);
        let secured = !matches!(
            (ap.wpa_flags().await, ap.rsn_flags().await),
            (
                Ok(NM80211ApSecurityFlags::NONE),
                Ok(NM80211ApSecurityFlags::NONE)
            )
        );
        match best.iter_mut().find(|n| n.ssid == ssid) {
            // One row per network, not one per radio: a mesh answers on several.
            Some(existing) => existing.strength = existing.strength.max(strength),
            None => best.push(Network {
                ssid,
                strength,
                secured,
            }),
        }
    }
    best.sort_by_key(|n| std::cmp::Reverse(n.strength));
    Ok(best)
}

/// Join a network on the uplink adapter, saving it so it comes back on its own next boot.
pub async fn join_network(wifi: &Wifi, ssid: &str, passphrase: &str) -> Result<()> {
    if ssid.trim().is_empty() {
        return Err(eyre!("pick a network first"));
    }
    let nm = get_network_manager().await;
    let device = role_device(WifiRole::Uplink, wifi.selector(WifiRole::Uplink)).await?;
    let iface = device.interface().await?;

    let mut profile = ConnectionProfile::new(ssid)
        .connection(
            ConnectionSetting::default()
                .id(ssid)
                .interface_name(&iface)
                // Unlike the accessory's profile, this one is meant to come back by itself.
                .autoconnect(true),
        )
        .wireless(WirelessSetting::default().ssid(ssid))
        .ipv4(Ipv4Setting::default().method(Ipv4Method::Auto))
        .ipv6(Ipv6Setting::default().method(Ipv6Method::Auto));
    if !passphrase.is_empty() {
        profile = profile.wireless_security(
            WirelessSecuritySetting::default()
                .key_mgmt(WirelessSecurityKeyMgmt::WpaPsk)
                .psk(passphrase),
        );
    }

    nm.add_and_activate_connection(&profile, &device, None)
        .await?;
    tracing::info!(%ssid, %iface, "Joining network on the uplink adapter");
    Ok(())
}
