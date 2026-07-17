use eyre::{Result, eyre};
use catplay_hap::{
    pair_setup::{PairSetup, CARPLAY_MAGIC_PIN},
    pair_verify::PairVerify,
    tlv,
    HomekitStorageFile,
};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

const AIRPLAY_PORT: u16 = 7000;
const BT_MAC: &str = "4C:79:6E:DA:29:4B";

async fn read_rtsp_response<R: AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> Result<(String, Vec<u8>)> {
    let mut response = String::new();
    let mut content_length: usize = 0;

    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Err(eyre!("Connection closed while reading response"));
        }
        response.push_str(&line);
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some(val) = line.strip_prefix("Content-Length:") {
            content_length = val.trim().parse().unwrap_or(0);
        }
    }

    let body = if content_length > 0 {
        let mut b = vec![0u8; content_length];
        reader.read_exact(&mut b).await?;
        b
    } else {
        Vec::new()
    };

    Ok((response, body))
}

async fn send_request<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    reader: &mut BufReader<R>,
    writer: &mut W,
    head: &str,
    body: &[u8],
) -> Result<(String, Vec<u8>)> {
    writer.write_all(head.as_bytes()).await?;
    if !body.is_empty() {
        writer.write_all(body).await?;
    }
    read_rtsp_response(reader).await
}

fn hex_preview(data: &[u8], max: usize) -> String {
    let n = std::cmp::min(data.len(), max);
    data[..n].iter().map(|b| format!("{:02x}", b)).collect()
}

fn apple_headers(cseq: u32) -> String {
    format!(
        "CSeq: {}\r\n\
         X-Apple-Device-ID: {}\r\n\
         User-Agent: AirPlay/320.14\r\n\
         X-Apple-ProtocolVersion: 1\r\n",
        cseq, BT_MAC)
}

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
    let mut cseq = 0u32;
    let mut nseq = || -> String { cseq += 1; apple_headers(cseq) };

    // ============== OPTIONS ==============
    let (resp, _) = send_request(&mut reader, &mut writer,
        &format!("OPTIONS * RTSP/1.0\r\n{}\r\n", nseq()), &[]).await?;
    tracing::info!("OPTIONS: {}", resp.lines().next().unwrap_or(""));

    // ============== TEARDOWN (reset) ==============
    let (resp, _) = send_request(&mut reader, &mut writer,
        &format!("TEARDOWN * RTSP/1.0\r\n{}\r\n", nseq()), &[]).await?;
    tracing::info!("TEARDOWN: {}", resp.lines().next().unwrap_or(""));

    // Create HomekitStorage with random identity
    let homekit = HomekitStorageFile::memory();

    // ============== Pair-Setup ==============
    let (ps_session, ps_m1) = PairSetup::client(CARPLAY_MAGIC_PIN);
    let head = format!(
        "POST /pair-setup RTSP/1.0\r\n\
         Content-Type: application/pairing+tlv8\r\n\
         X-Apple-HKP: 2\r\n\
         Content-Length: {}\r\n\
         {}\r\n",
        ps_m1.len(), nseq());
    let (resp, body) = send_request(&mut reader, &mut writer, &head, &ps_m1).await?;
    tracing::warn!("/pair-setup M1: {} [b:{}b {}]",
        resp.lines().next().unwrap_or(""), body.len(), hex_preview(&body, 48));

    let (ps_session, ps_m3) = ps_session.handle(&*homekit, &body)
        .map_err(|(e, _)| eyre!("Pair-setup M2->M3: {:?}", e))?;
    let head = format!(
        "POST /pair-setup RTSP/1.0\r\n\
         Content-Type: application/pairing+tlv8\r\n\
         X-Apple-HKP: 2\r\n\
         Content-Length: {}\r\n\
         {}\r\n",
        ps_m3.len(), nseq());
    let (resp, body) = send_request(&mut reader, &mut writer, &head, &ps_m3).await?;
    tracing::warn!("/pair-setup M3: {} [b:{}b {}]",
        resp.lines().next().unwrap_or(""), body.len(), hex_preview(&body, 48));

    // Check for error in M4
    let decoded = tlv::decode(&body);
    if let Some(err_val) = decoded.get(&0x07) {
        tracing::error!("Car returned Error {} in M4", err_val[0]);
        return Err(eyre!("M4 error code {}", err_val[0]));
    }

    let (ps_session, ps_m5) = ps_session.handle(&*homekit, &body)
        .map_err(|(e, _)| eyre!("Pair-setup M4->M5: {:?}", e))?;
    let head = format!(
        "POST /pair-setup RTSP/1.0\r\n\
         Content-Type: application/pairing+tlv8\r\n\
         X-Apple-HKP: 2\r\n\
         Content-Length: {}\r\n\
         {}\r\n",
        ps_m5.len(), nseq());
    let (resp, body) = send_request(&mut reader, &mut writer, &head, &ps_m5).await?;
    tracing::warn!("/pair-setup M5: {} [b:{}b {}]",
        resp.lines().next().unwrap_or(""), body.len(), hex_preview(&body, 48));

    let (_, _) = ps_session.handle(&*homekit, &body)
        .map_err(|(e, _)| eyre!("Pair-setup M6->Finished: {:?}", e))?;
    tracing::info!("Pair-setup complete!");

    // ============== Pair-Verify ==============
    let (pv_session, pv_m1) = PairVerify::client()
        .map_err(|_| eyre!("PairVerify::client failed"))?;
    let head = format!(
        "POST /pair-verify RTSP/1.0\r\n\
         Content-Type: application/pairing+tlv8\r\n\
         X-Apple-HKP: 2\r\n\
         Content-Length: {}\r\n\
         {}\r\n",
        pv_m1.len(), nseq());
    let (resp, body) = send_request(&mut reader, &mut writer, &head, &pv_m1).await?;
    tracing::warn!("/pair-verify M1: {} [b:{}b]",
        resp.lines().next().unwrap_or(""), body.len());

    let (pv_session, pv_m3) = pv_session.handle(&*homekit, &body)
        .map_err(|(e, _)| eyre!("Pair-verify M2->M3: {:?}", e))?;
    let head = format!(
        "POST /pair-verify RTSP/1.0\r\n\
         Content-Type: application/pairing+tlv8\r\n\
         X-Apple-HKP: 2\r\n\
         Content-Length: {}\r\n\
         {}\r\n",
        pv_m3.len(), nseq());
    let (resp, body) = send_request(&mut reader, &mut writer, &head, &pv_m3).await?;
    tracing::warn!("/pair-verify M3: {} [b:{}b]",
        resp.lines().next().unwrap_or(""), body.len());

    let (_, _) = pv_session.handle(&*homekit, &body)
        .map_err(|(e, _)| eyre!("Pair-verify M4->Finished: {:?}", e))?;
    tracing::info!("Pair-verify complete!");

    // ============== GET /info ==============
    let (resp, body) = send_request(&mut reader, &mut writer,
        &format!("GET /info RTSP/1.0\r\n{}\r\n", nseq()), &[]).await?;
    tracing::warn!("/info: {} [b:{}b {}]",
        resp.lines().next().unwrap_or(""), body.len(), hex_preview(&body, 96));

    // ============== SETUP ==============
    let (resp, body) = send_request(&mut reader, &mut writer,
        &format!("SETUP * RTSP/1.0\r\n\
            Transport: RTP/AVP/TCP;unicast;interleaved=0-1;mode=record\r\n\
            {}\r\n", nseq()), &[]).await?;
    tracing::info!("SETUP: {} [b:{}b {}]",
        resp.lines().next().unwrap_or(""), body.len(), hex_preview(&body, 64));

    // ============== RECORD ==============
    let (resp, _) = send_request(&mut reader, &mut writer,
        &format!("RECORD * RTSP/1.0\r\nSession: 1\r\n{}\r\n", nseq()), &[]).await?;
    tracing::info!("RECORD: {}", resp.lines().next().unwrap_or(""));

    // ============== Send video data ==============
    tracing::info!("Sending video data");
    let mut header = [0u8; 4];
    while let Some(data) = video_rx.recv().await {
        let mut offset = 0;
        while offset < data.len() {
            let remaining = data.len() - offset;
            let chunk_len = if remaining > 0xFFFF {
                let search_start = std::cmp::min(offset + 12, data.len());
                let search_end = std::cmp::min(offset + 0xFFFF, data.len());
                match data[search_start..search_end].iter().position(|&b| b == 0x80) {
                    Some(pos) if pos > 0 => search_start + pos - offset,
                    _ => 0xFFFF,
                }
            } else {
                remaining
            };
            if chunk_len > 0xFFFF { continue; }
            header[0] = 0x24;
            header[1] = 0x00;
            header[2] = (chunk_len >> 8) as u8;
            header[3] = (chunk_len & 0xFF) as u8;
            if let Err(e) = writer.write_all(&header).await {
                tracing::error!("write error: {e}");
                return Err(e.into());
            }
            if let Err(e) = writer.write_all(&data[offset..offset + chunk_len]).await {
                tracing::error!("write error: {e}");
                return Err(e.into());
            }
            offset += chunk_len;
        }
    }

    tracing::info!("Video stream ended");
    Ok(())
}
