use backon::{ConstantBuilder, Retryable};
use enclose::enclose;
use eyre::{ErrReport, Result, eyre};
use ipnet::IpNet;
use nmrs::NetworkManager;
use nmrs::builders::{WifiBand, WifiConnectionBuilder};
use std::net::IpAddr;
use std::time::Duration;
use tokio::sync::OnceCell;
use zbus::{proxy, zvariant::OwnedObjectPath};

static NETWORK_MANAGER: OnceCell<NetworkManager> = OnceCell::const_new();

pub(crate) async fn get_network_manager() -> &'static NetworkManager {
    NETWORK_MANAGER
        .get_or_init(async || {
            NetworkManager::new()
                .await
                .expect("Unable to get networkmanager")
        })
        .await
}

pub struct CarWifi {
    #[allow(dead_code)]
    pub ssid: String,
    #[allow(dead_code)]
    pub ip: IpAddr,
    pub gateway: IpAddr,
    pub iface: String,
    pub iface_mac: String,
}

pub(crate) async fn connect_to_car_wifi(ssid: &str, passphrase: &str) -> Result<CarWifi> {
    let settings = WifiConnectionBuilder::new(ssid)
        .wpa_psk(passphrase)
        .band(WifiBand::A)
        .hidden(true)
        .ipv4_auto()
        .build();

    let car_wifi = enclose!(
        (settings)
            || async {
                let (conn_path, active_path) = get_network_manager()
                    .await
                    .add_and_activate_connection(settings, None, None)
                    .await?;
                tracing::info!("Connection created: {}, active: {}", conn_path, active_path);
                let car_wifi = get_car_wifi(active_path).await?;
                Ok::<_, ErrReport>(car_wifi)
            }
    )
    .retry(ConstantBuilder::new().with_delay(Duration::from_secs(2)))
    .await?;

    Ok(car_wifi)
}

async fn get_car_wifi(active_path: OwnedObjectPath) -> Result<CarWifi> {
    let network = get_network_manager()
        .await
        .current_network()
        .await?
        .ok_or(eyre!("No network connection"))?;
    let addr = network
        .ip4_address
        .ok_or(eyre!("No address on current connection"))?;
    let ipnet = addr.parse::<IpNet>().expect("Error parsing addr");
    let ip = ipnet.addr();

    let gateway = get_gateway(active_path).await?;

    Ok(CarWifi {
        ssid: network.ssid,
        ip,
        iface: network.device,
        iface_mac: get_wifi_mac().await?,
        gateway,
    })
}

pub(crate) async fn get_wifi_mac() -> Result<String> {
    let network = get_network_manager()
        .await
        .current_network()
        .await?
        .ok_or(eyre!("No network connection"))?;
    let device = get_network_manager()
        .await
        .wifi_device_by_interface(&network.device)
        .await?;
    Ok(device.hw_address)
}

#[proxy(
    interface = "org.freedesktop.NetworkManager.Connection.Active",
    default_service = "org.freedesktop.NetworkManager"
)]
trait ActiveConnection {
    #[zbus(property)]
    fn ip4_config(&self) -> zbus::Result<OwnedObjectPath>;
}
#[proxy(
    interface = "org.freedesktop.NetworkManager.IP4Config",
    default_service = "org.freedesktop.NetworkManager"
)]
trait IP4Config {
    #[zbus(property)]
    fn gateway(&self) -> zbus::Result<String>;
}

async fn get_gateway(active_path: OwnedObjectPath) -> Result<IpAddr> {
    let conn = get_network_manager().await.dbus_connection();
    let active_connection_proxy = ActiveConnectionProxy::new(conn, active_path).await?;
    let ip4_config_path = active_connection_proxy.ip4_config().await?;
    tracing::debug!(%ip4_config_path);
    let ip4_config_proxy = IP4ConfigProxy::new(conn, ip4_config_path).await?;
    let gateway: IpAddr = ip4_config_proxy.gateway().await?.parse()?;
    tracing::debug!(?gateway);
    Ok(gateway)
}
