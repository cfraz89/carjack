use nmrs::builders::{WifiBand, WifiConnectionBuilder};
use nmrs::NetworkManager;

#[tokio::main]
async fn main() {
    let nm = NetworkManager::new().await.unwrap();
    let ssid = "test_ssid";
    let passphrase = "test123";
    
    let mut settings = WifiConnectionBuilder::new(ssid)
        .wpa_psk(passphrase)
        .band(WifiBand::A)
        .hidden(true)
        .ipv4_auto()
        .build();
    
    eprintln!("device_type: {:?}", 
        settings.get("connection").and_then(|c| c.get("type")));
    eprintln!("has 802-11-wireless: {}", 
        settings.contains_key("802-11-wireless"));
    eprintln!("all keys: {:?}", 
        settings.keys().collect::<Vec<_>>());
    
    // Now try the NM call
    match nm.add_and_activate_connection(settings, None, None).await {
        Ok(_) => eprintln!("SUCCESS"),
        Err(e) => eprintln!("ERROR: {e}"),
    }
}
