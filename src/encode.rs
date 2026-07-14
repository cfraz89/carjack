use eyre::Result;
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;

pub async fn encode_h264_to_channel(
    node_id: u32,
    video_tx: mpsc::Sender<Vec<u8>>,
) -> Result<()> {
    let encoder = std::env::var("CARPLAY_ENCODER").unwrap_or_else(|_| String::from("x264enc"));

    let pipeline = format!(
        "pipewiresrc path={} ! \
         video/x-raw,framerate=30/1 ! \
         videoconvert ! \
         {} ! \
         h264parse ! \
         fdsink fd=1 sync=false",
        node_id, encoder
    );

    tracing::info!("Starting GStreamer pipeline: {}", pipeline);

    let mut child = tokio::process::Command::new("gst-launch-1.0")
        .args(["-e", &pipeline])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or(eyre::eyre!("No stdout from gst-launch-1.0"))?;

    let mut buf = vec![0u8; 65536];
    loop {
        let n = stdout.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        if video_tx.send(buf[..n].to_vec()).await.is_err() {
            tracing::warn!("Video channel closed, terminating pipeline");
            let _ = child.kill().await;
            break;
        }
    }

    let status = child.wait().await?;
    tracing::info!("GStreamer pipeline exited: {}", status);
    Ok(())
}
