use eyre::{Result, eyre};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::time::{timeout, Duration};
use zbus::Connection;

/// Advertise _carplay-ctrl._tcp via mDNS and wait for the car's probe request.
pub async fn wait_for_carplay_ctrl_probe() -> Result<()> {
    let listen_port: u16 = 8082;
    let listener = TcpListener::bind(format!("0.0.0.0:{}", listen_port)).await?;
    tracing::info!("CarPlay Ctrl listening on port {}", listen_port);

    // Register _carplay-ctrl._tcp with avahi
    if let Err(e) = register_carplay_ctrl(listen_port).await {
        tracing::warn!("avahi registration failed (non-fatal): {e}");
    } else {
        tracing::info!("_carplay-ctrl._tcp registered via avahi");
    }

    // Wait for the car to probe us (timeout after 15s)
    match timeout(Duration::from_secs(15), listener.accept()).await {
        Ok(Ok((mut stream, addr))) => {
            tracing::info!("Car connected from {}:{}", addr.ip(), addr.port());

            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).await?;
            let req = String::from_utf8_lossy(&buf[..n]);
            tracing::info!("Probe request: {}", req.lines().next().unwrap_or("?"));

            if req.contains("GET /ctrl-int/1/connect") {
                let resp = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                stream.write_all(resp.as_bytes()).await?;
                tracing::info!("Responded 200 OK to car probe");
                Ok(())
            } else {
                tracing::warn!("Unexpected request: {}", req.lines().next().unwrap_or(""));
                Err(eyre!("Unexpected probe request"))
            }
        }
        Ok(Err(e)) => Err(eyre!("Accept error: {e}")),
        Err(_) => Err(eyre!("Timeout waiting for car probe (15s)")),
    }
}

async fn register_carplay_ctrl(port: u16) -> Result<()> {
    let conn = Connection::system().await?;

    // Create entry group
    let group_path: zbus::zvariant::OwnedObjectPath = conn
        .call_method(
            Some("org.freedesktop.Avahi"),
            "/",
            Some("org.freedesktop.Avahi.Server"),
            "EntryGroupNew",
            &(),
        )
        .await?
        .body()
        .deserialize()?;

    let name = format!("carjack-{}", std::process::id());
    let txt_records: Vec<Vec<u8>> = vec![b"id=4C:79:6E:DA:29:4B".to_vec()];

    // Add service to group
    let _: () = conn
        .call_method(
            Some("org.freedesktop.Avahi"),
            group_path.clone(),
            Some("org.freedesktop.Avahi.EntryGroup"),
            "AddService",
            &(-1i32, -1i32, 0u32, &name, "_carplay-ctrl._tcp", "", "", port, txt_records),
        )
        .await?
        .body()
        .deserialize()?;

    // Commit
    let _: () = conn
        .call_method(
            Some("org.freedesktop.Avahi"),
            group_path,
            Some("org.freedesktop.Avahi.EntryGroup"),
            "Commit",
            &(),
        )
        .await?
        .body()
        .deserialize()?;

    Ok(())
}
