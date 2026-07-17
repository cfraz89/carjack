use eyre::Result;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

const AIRPLAY_PORT: u16 = 7000;

pub async fn start_video_stream(
    car_ip: std::net::IpAddr,
    mut video_rx: mpsc::Receiver<Vec<u8>>,
) -> Result<()> {
    let addr = format!("{}:{}", car_ip, AIRPLAY_PORT);
    tracing::info!("Connecting to AirPlay server at {}", addr);

    let stream = TcpStream::connect(&addr).await?;
    tracing::info!("Connected to car AirPlay port {}", AIRPLAY_PORT);

    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut buf = String::new();

    loop {
        // Wait for RTSP request from car
        buf.clear();
        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                tracing::info!("AirPlay connection closed");
                return Ok(());
            }
            if line == "\r\n" || line == "\n" {
                break;
            }
            buf.push_str(&line);
        }

        tracing::debug!("AirPlay RTSP request:\n{}", buf);

        if buf.starts_with("ANNOUNCE") {
            writer.write_all(b"RTSP/1.0 200 OK\r\n\r\n").await?;
            tracing::info!("AirPlay: responded to ANNOUNCE");
        } else if buf.starts_with("SETUP") {
            writer.write_all(b"RTSP/1.0 200 OK\r\n\r\n").await?;
            tracing::info!("AirPlay: responded to SETUP");
        } else if buf.starts_with("PLAY") {
            writer.write_all(b"RTSP/1.0 200 OK\r\n\r\n").await?;
            tracing::info!("AirPlay: responded to PLAY, starting video stream");
            break;
        } else {
            writer.write_all(b"RTSP/1.0 200 OK\r\n\r\n").await?;
            tracing::debug!("AirPlay: responded to unknown request");
        }
    }

    // Send video frames
    while let Some(data) = video_rx.recv().await {
        if let Err(e) = writer.write_all(&data).await {
            tracing::error!("AirPlay write error: {e}");
            return Err(e.into());
        }
    }

    tracing::info!("Video stream ended");
    Ok(())
}
