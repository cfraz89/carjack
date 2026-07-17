use eyre::{Result, eyre};
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305};
use ring::hmac;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use x25519_dalek::{EphemeralSecret, PublicKey as XPublicKey};

use crate::pairing;

const AIRPLAY_PORT: u16 = 7000;
const BT_MAC: &str = "AA:BB:CC:DD:EE:04";
const CARPLAY_USER: &str = "CarPlay";

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

fn hkdf_sha512(salt: &[u8], ikm: &[u8], info: &[u8], out_len: usize) -> Vec<u8> {
    let salt_key = hmac::Key::new(hmac::HMAC_SHA512, salt);
    let prk = hmac::sign(&salt_key, ikm);
    let prk_key = hmac::Key::new(hmac::HMAC_SHA512, prk.as_ref());
    let mut expand_input = Vec::new();
    expand_input.extend_from_slice(info);
    expand_input.push(0x01);
    let okm = hmac::sign(&prk_key, &expand_input);
    okm.as_ref()[..out_len].to_vec()
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
    send_request(&mut reader, &mut writer,
        &format!("OPTIONS * RTSP/1.0\r\n{}\r\n", nseq()), &[]).await?;
    tracing::info!("OPTIONS sent");

    // ============== TEARDOWN (reset) ==============
    let (resp, _) = send_request(&mut reader, &mut writer,
        &format!("TEARDOWN * RTSP/1.0\r\n{}\r\n", nseq()), &[]).await?;
    tracing::info!("TEARDOWN: {}", resp.lines().next().unwrap_or(""));

    // ============== Pair-Setup M1 ==============
    let m1_body = pairing::pair_setup_m1();
    let head = format!(
        "POST /pair-setup RTSP/1.0\r\n\
         Content-Type: application/pairing+tlv8\r\n\
         X-Apple-HKP: 2\r\n\
         Content-Length: {}\r\n\
         {}\r\n",
        m1_body.len(), nseq());
    let (resp, body) = send_request(&mut reader, &mut writer, &head, &m1_body).await?;
    tracing::warn!("/pair-setup M1: {} [b:{}b {}]",
        resp.lines().next().unwrap_or(""), body.len(), hex_preview(&body, 96));

    // ============== Pair-Setup M2 -> SRP ==============
    let (salt, B) = pairing::parse_m2(&body)
        .map_err(|e| eyre!("Failed to parse M2: {e}"))?;
    tracing::info!("M2: salt_len={}, B_len={}", salt.len(), B.len());

    let (A_padded, M1_proof, expected_M2, K) = pairing::do_srp(&salt, &B);
    tracing::info!("SRP computed: A={}.., M1={}.., K_len={}",
        hex_preview(&A_padded, 16), hex_preview(&M1_proof, 16), K.len());

    // ============== Pair-Setup M3 ==============
    let m3_body = pairing::pair_setup_m3(&A_padded, &M1_proof);
    let head = format!(
        "POST /pair-setup RTSP/1.0\r\n\
         Content-Type: application/pairing+tlv8\r\n\
         X-Apple-HKP: 2\r\n\
         Content-Length: {}\r\n\
         {}\r\n",
        m3_body.len(), nseq());
    let (resp, body) = send_request(&mut reader, &mut writer, &head, &m3_body).await?;
    tracing::warn!("/pair-setup M3: {} [b:{}b {}]",
        resp.lines().next().unwrap_or(""), body.len(), hex_preview(&body, 96));

    // ============== Pair-Setup M4 (verify) ==============
    let server_proof = pairing::parse_m4(&body)
        .map_err(|e| eyre!("Failed to parse M4: {e}"))?;

    if server_proof == expected_M2 {
        tracing::info!("Pair-setup M4 verified OK!");
    } else {
        tracing::error!("Server proof mismatch in M4!");
        tracing::error!("  expected: {}", hex_preview(&expected_M2, 64));
        tracing::error!("  received: {}", hex_preview(&server_proof, 64));
    }

    // ============== Pair-Setup M5 ==============
    let rng = SystemRandom::new();
    let pkcs8_bytes = Ed25519KeyPair::generate_pkcs8(&rng)
        .map_err(|_| eyre!("Ed25519 keygen failed"))?;
    let our_ed = Ed25519KeyPair::from_pkcs8(pkcs8_bytes.as_ref())
        .map_err(|_| eyre!("Ed25519 key parse failed"))?;
    let our_ed_pub = our_ed.public_key().as_ref().to_vec();

    let id_bytes = CARPLAY_USER.as_bytes();
    let mut to_sign = Vec::new();
    to_sign.extend_from_slice(id_bytes);
    to_sign.extend_from_slice(&our_ed_pub);
    to_sign.extend_from_slice(&K);
    let sig = our_ed.sign(&to_sign);

    let inner = pairing::tlv8_build(&[
        (0x01, id_bytes.to_vec()),
        (0x03, our_ed_pub),
        (0x00, sig.as_ref().to_vec()),
    ]);

    let enc_key = &K[..32];
    let unbound = UnboundKey::new(&CHACHA20_POLY1305, enc_key)
        .map_err(|_| eyre!("Bad ChaCha20 key"))?;
    let enc_key_obj = LessSafeKey::new(unbound);
    let nonce = Nonce::assume_unique_for_key([0u8; 12]);
    let mut ciphertext = inner;
    enc_key_obj.seal_in_place_append_tag(nonce, Aad::empty(), &mut ciphertext)
        .map_err(|_| eyre!("M5 encryption failed"))?;

    let m5_body = pairing::tlv8_build(&[
        (0x06, vec![0x05]),
        (0x05, ciphertext),
    ]);
    let head = format!(
        "POST /pair-setup RTSP/1.0\r\n\
         Content-Type: application/pairing+tlv8\r\n\
         X-Apple-HKP: 2\r\n\
         Content-Length: {}\r\n\
         {}\r\n",
        m5_body.len(), nseq());
    let (resp, body) = send_request(&mut reader, &mut writer, &head, &m5_body).await?;
    tracing::warn!("/pair-setup M5: {} [b:{}b {}]",
        resp.lines().next().unwrap_or(""), body.len(), hex_preview(&body, 96));

    // ============== Pair-Setup M6 ==============
    let entries = pairing::tlv8_parse(&body);
    let state = pairing::tlv8_get(&entries, 0x06)
        .ok_or(eyre!("Missing State in M6"))?;
    if state.len() != 1 || state[0] != 6 {
        tracing::warn!("M6 unexpected state: {:?}", state);
    }
    let enc_data = pairing::tlv8_get(&entries, 0x05)
        .ok_or(eyre!("Missing EncryptedData in M6"))?;

    let unbound = UnboundKey::new(&CHACHA20_POLY1305, enc_key)
        .map_err(|_| eyre!("Bad key for M6"))?;
    let dec_key = LessSafeKey::new(unbound);
    let nonce = Nonce::assume_unique_for_key([0u8; 12]);
    let mut m6_ct = enc_data.clone();
    let plaintext = dec_key.open_in_place(nonce, Aad::empty(), &mut m6_ct)
        .map_err(|_| eyre!("M6 decryption failed"))?;

    let inner_entries = pairing::tlv8_parse(plaintext);
    let car_id = pairing::tlv8_get(&inner_entries, 0x01);
    let car_pub = pairing::tlv8_get(&inner_entries, 0x03)
        .ok_or(eyre!("Missing car Ed25519 pubkey in M6"))?;
    let car_sig = pairing::tlv8_get(&inner_entries, 0x00)
        .ok_or(eyre!("Missing car signature in M6"))?;
    tracing::info!("Pair-setup M6: car_pub={}", hex_preview(&car_pub, 32));

    let car_id_bytes = car_id.as_deref().unwrap_or(b"");
    let mut to_verify = Vec::new();
    to_verify.extend_from_slice(car_id_bytes);
    to_verify.extend_from_slice(&car_pub);
    to_verify.extend_from_slice(&K);
    let peer_pubkey = ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, &car_pub);
    if peer_pubkey.verify(&to_verify, &car_sig).is_err() {
        tracing::warn!("Car M6 signature verification FAILED");
    } else {
        tracing::info!("Car M6 signature verified!");
    }
    tracing::info!("Pair-setup complete!");

    // ============== Pair-Verify ==============
    let our_x_secret = EphemeralSecret::new(&mut rand::rngs::OsRng);
    let our_x_pub = XPublicKey::from(&our_x_secret);
    let our_x_pub_bytes = our_x_pub.as_bytes().to_vec();

    // Pair-Verify M1
    let pv_m1_body = pairing::tlv8_build(&[
        (0x00, vec![0x00]),
        (0x06, vec![0x01]),
        (0x03, our_x_pub_bytes.clone()),
    ]);
    let head = format!(
        "POST /pair-verify RTSP/1.0\r\n\
         Content-Type: application/pairing+tlv8\r\n\
         X-Apple-HKP: 2\r\n\
         Content-Length: {}\r\n\
         {}\r\n",
        pv_m1_body.len(), nseq());
    let (resp, body) = send_request(&mut reader, &mut writer, &head, &pv_m1_body).await?;
    let status = resp.lines().next().unwrap_or("").to_string();
    tracing::warn!("/pair-verify M1: {} [b:{}b {}]",
        status, body.len(), hex_preview(&body, 96));

    // Pair-Verify M2
    let pv_entries = pairing::tlv8_parse(&body);
    let pv_state = pairing::tlv8_get(&pv_entries, 0x06)
        .ok_or(eyre!("Missing State in PV M2"))?;
    if pv_state.len() != 1 || pv_state[0] != 2 {
        return Err(eyre!("Unexpected PV M2 state: {:?}", pv_state));
    }
    let car_x_pub_bytes = pairing::tlv8_get(&pv_entries, 0x03)
        .ok_or(eyre!("Missing X25519 pubkey in PV M2"))?;
    let pv_enc_data = pairing::tlv8_get(&pv_entries, 0x05)
        .ok_or(eyre!("Missing EncryptedData in PV M2"))?;

    let car_x_pub = XPublicKey::from(<[u8; 32]>::try_from(car_x_pub_bytes.as_slice())
        .map_err(|_| eyre!("Invalid X25519 pubkey length"))?);
    let shared_secret = our_x_secret.diffie_hellman(&car_x_pub);
    let ss_bytes = shared_secret.as_bytes().to_vec();

    let pv_salt = b"Pair-Verify-Encrypt-Salt";
    let pv_info = b"Pair-Verify-Encrypt-Info";
    let pv_session_key = hkdf_sha512(pv_salt, &ss_bytes, pv_info, 32);
    tracing::info!("PV session key: {}", hex_preview(&pv_session_key, 16));

    // Decrypt M2's EncryptedData
    let unbound = UnboundKey::new(&CHACHA20_POLY1305, &pv_session_key)
        .map_err(|_| eyre!("Bad PV decrypt key"))?;
    let pv_dec_key = LessSafeKey::new(unbound);
    let nonce = Nonce::assume_unique_for_key([0u8; 12]);
    let mut pv_ct = pv_enc_data.clone();
    let pv_plaintext = pv_dec_key.open_in_place(nonce, Aad::empty(), &mut pv_ct)
        .map_err(|_| eyre!("PV M2 decryption failed"))?;

    let pv_inner = pairing::tlv8_parse(pv_plaintext);
    let pv_car_id = pairing::tlv8_get(&pv_inner, 0x01)
        .ok_or(eyre!("Missing car ID in PV M2"))?;
    let pv_car_sig = pairing::tlv8_get(&pv_inner, 0x00)
        .ok_or(eyre!("Missing car sig in PV M2"))?;

    // Verify car's Ed25519 signature
    let mut pv_to_verify = Vec::new();
    pv_to_verify.extend_from_slice(&our_x_pub_bytes);
    pv_to_verify.extend_from_slice(&car_x_pub_bytes);
    pv_to_verify.extend_from_slice(b"Pair-Verify");
    pv_to_verify.extend_from_slice(&pv_car_id);

    let pv_car_pubkey = ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, &car_pub);
    if pv_car_pubkey.verify(&pv_to_verify, &pv_car_sig).is_err() {
        tracing::warn!("PV M2: car signature verification FAILED");
    } else {
        tracing::info!("PV M2: car signature verified!");
    }

    // Pair-Verify M3: send our encrypted proof
    let our_id_bytes = CARPLAY_USER.as_bytes();
    let mut pv_our_to_sign = Vec::new();
    pv_our_to_sign.extend_from_slice(&our_x_pub_bytes);
    pv_our_to_sign.extend_from_slice(&car_x_pub_bytes);
    pv_our_to_sign.extend_from_slice(b"Pair-Verify");
    pv_our_to_sign.extend_from_slice(our_id_bytes);
    let pv_our_sig = our_ed.sign(&pv_our_to_sign);

    let pv_inner_m3 = pairing::tlv8_build(&[
        (0x01, our_id_bytes.to_vec()),
        (0x00, pv_our_sig.as_ref().to_vec()),
    ]);

    let unbound = UnboundKey::new(&CHACHA20_POLY1305, &pv_session_key)
        .map_err(|_| eyre!("Bad PV encrypt key"))?;
    let pv_enc_key = LessSafeKey::new(unbound);
    let nonce = Nonce::assume_unique_for_key([0u8; 12]);
    let mut pv_m3_ct = pv_inner_m3;
    pv_enc_key.seal_in_place_append_tag(nonce, Aad::empty(), &mut pv_m3_ct)
        .map_err(|_| eyre!("PV M3 encryption failed"))?;

    let pv_m3_body = pairing::tlv8_build(&[
        (0x06, vec![0x03]),
        (0x05, pv_m3_ct),
    ]);
    let head = format!(
        "POST /pair-verify RTSP/1.0\r\n\
         Content-Type: application/pairing+tlv8\r\n\
         X-Apple-HKP: 2\r\n\
         Content-Length: {}\r\n\
         {}\r\n",
        pv_m3_body.len(), nseq());
    let (resp, body) = send_request(&mut reader, &mut writer, &head, &pv_m3_body).await?;
    tracing::warn!("/pair-verify M3: {} [b:{}b {}]",
        resp.lines().next().unwrap_or(""), body.len(), hex_preview(&body, 64));

    // Pair-Verify M4
    let pv_final = pairing::tlv8_parse(&body);
    let pv_final_state = pairing::tlv8_get(&pv_final, 0x06)
        .ok_or(eyre!("Missing State in PV M4"))?;
    if pv_final_state.len() == 1 && pv_final_state[0] == 4 {
        tracing::info!("Pair-verify complete! State=4");
    } else {
        tracing::warn!("PV M4 unexpected state: {:?}", pv_final_state);
    }

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
