use eyre::{Result, eyre};
use nmrs::NetworkManager;
use nmrs::builders::{WifiBand, WifiConnectionBuilder};
use std::net::IpAddr;
use std::time::Duration;
use tokio::time::sleep;

pub struct CarWifi {
    #[allow(dead_code)]
    pub ssid: String,
    pub ip: IpAddr,
    pub iface: String,
}

pub async fn connect_to_car_wifi(ssid: &str, passphrase: &str) -> Result<CarWifi> {
    let nm = NetworkManager::new().await?;

    try_nm_connect(&nm, ssid, passphrase).await
}

async fn try_nm_connect(nm: &NetworkManager, ssid: &str, passphrase: &str) -> Result<CarWifi> {
    for attempt in 0..10 {
        let settings = WifiConnectionBuilder::new(ssid)
            .wpa_psk(passphrase)
            .band(WifiBand::A)
            .hidden(true)
            .ipv4_auto()
            .build();

        tracing::info!(
            "NM add_and_activate_connection attempt {} for SSID={}",
            attempt + 1,
            ssid
        );

        match nm.add_and_activate_connection(settings, None, None).await {
            Ok((conn_path, active_path)) => {
                tracing::info!("Connection created: {}, active: {}", conn_path, active_path);
                sleep(Duration::from_secs(2)).await;
                if let Ok((ip, iface)) = get_car_ip(&nm).await {
                    tracing::info!("Car WiFi connected via NM, IP: {} on {}", ip, iface);
                    return Ok(CarWifi {
                        ssid: ssid.to_string(),
                        ip: guess_gateway(ip),
                        iface,
                    });
                }
                tracing::warn!("No IP yet, will retry");
            }
            Err(e) => {
                tracing::warn!("NM attempt {} failed: {e:#}", attempt + 1);
            }
        }
        sleep(Duration::from_secs(3)).await;
    }

    Err(eyre!("Failed to connect to car WiFi after 10 attempts"))
}

async fn get_car_ip(nm: &NetworkManager) -> Result<(IpAddr, String)> {
    let devices = nm.list_devices().await?;
    for dev in &devices {
        if dev.device_type == nmrs::DeviceType::Wifi && dev.state == nmrs::DeviceState::Activated {
            if let Some(ref ip_str) = dev.ip4_address {
                if let Some(slash) = ip_str.find('/') {
                    if let Ok(ip) = ip_str[..slash].parse::<IpAddr>() {
                        return Ok((ip, dev.interface.clone()));
                    }
                }
            }
        }
    }
    Err(eyre!("No WiFi device with IP"))
}

fn guess_gateway(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            IpAddr::V4(std::net::Ipv4Addr::new(octets[0], octets[1], octets[2], 1))
        }
        IpAddr::V6(_) => IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED),
    }
}
