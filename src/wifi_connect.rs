use eyre::{Result, eyre};
use nmrs::{NetworkManager, WifiSecurity};
use std::net::IpAddr;
use std::time::Duration;
use tokio::time::sleep;

pub struct CarWifi {
    #[allow(dead_code)]
    pub ssid: String,
    pub ip: IpAddr,
}

pub async fn connect_to_car_wifi(ssid: &str, passphrase: &str) -> Result<CarWifi> {
    let nm = NetworkManager::new().await?;

    tracing::info!("Connecting to car WiFi via nmrs: {}", ssid);

    nm.connect(ssid, None, WifiSecurity::WpaPsk {
        psk: passphrase.to_string(),
    }).await.map_err(|e| eyre!("WiFi connection failed: {e}"))?;

    // Wait for IP assignment
    sleep(Duration::from_secs(5)).await;

    // Get our IP on the car's network
    let devices = nm.list_devices().await?;
    let mut our_ip: Option<IpAddr> = None;
    for dev in &devices {
        if dev.device_type == nmrs::DeviceType::Wifi && dev.state == nmrs::DeviceState::Activated {
            if let Some(ref ip_str) = dev.ip4_address {
                if let Some(slash) = ip_str.find('/') {
                    if let Ok(ip) = ip_str[..slash].parse::<IpAddr>() {
                        our_ip = Some(ip);
                        tracing::info!("Car WiFi connected, our IP: {}", &ip_str[..slash]);
                    }
                }
            }
        }
    }

    let ip = our_ip.ok_or_else(|| eyre!("No IP address assigned on car WiFi"))?;

    let gateway = match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            IpAddr::V4(std::net::Ipv4Addr::new(octets[0], octets[1], octets[2], 1))
        }
        IpAddr::V6(_) => return Err(eyre!("IPv6 not supported for car WiFi")),
    };

    tracing::info!("Car gateway IP: {}", gateway);

    Ok(CarWifi {
        ssid: ssid.to_string(),
        ip: gateway,
    })
}
