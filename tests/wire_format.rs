//! The bytes the accessory actually receives, built from real encoder output rather than from
//! fixtures: the sample entry of a config frame, and the sample of the keyframe behind it.
//!
//! The reference is an iPhone's own config frame to a head unit - a receiver sets its decoder up
//! from these bytes and from nothing else, so anything the encoder or the parser changes here is
//! worth failing a test over. Ignored by default: it needs the GPU the encoder runs on.
//!
//! ```text
//! cargo test --test wire_format -- --ignored --nocapture
//! ```

use carjack::compositor::Compositor;
use carjack::config::{Cli, Config};
use carjack::encode::{Bitrate, encode_h264_to_channel, resolve_backend};
use catplay_carplay::clock::MediaClockSession;
use catplay_carplay::screen::{ScreenFrame, ScreenOpCode};
use catplay_carplay::video::{
    AvccConfigExtended, EncodedVideoFrame, HevcNalType, Pts, VideoView,
};
use clap::Parser;
use eyre::{Result, eyre};
use libcarjack::airplay::EncodedFrame;
use tokio::sync::mpsc;
use tokio_util::task::AbortOnDropHandle;

/// Walk a length-prefixed sample, returning each NAL's first byte, or `None` if the prefixes do
/// not describe the buffer exactly - which is what a receiver's parser would find.
fn nal_headers(sample: &[u8]) -> Option<Vec<u8>> {
    let mut headers = Vec::new();
    let mut pos = 0;
    while pos + 4 < sample.len() {
        let len = u32::from_be_bytes(sample[pos..pos + 4].try_into().ok()?) as usize;
        headers.push(*sample.get(pos + 4)?);
        pos += 4 + len;
    }
    (pos == sample.len()).then_some(headers)
}

async fn first_keyframe() -> Result<EncodedFrame> {
    let cfg = Config::load(&Cli::parse_from(["carjack"]))?;
    let node = carjack::config::resolve_render_node(&cfg.graphics.drm_device)?;
    // `true`: no accessory here to declare a codec, so `auto` keeps its preference.
    let backend = resolve_backend(
        cfg.graphics.zerocopy,
        &node,
        carjack::encode::resolve_codec(cfg.graphics.codec, true),
    )?;
    let comp = Compositor::start(
        node,
        backend.is_zero_copy(),
        carjack::encode::importable_modifiers(&backend),
        800,
        1280,
        30,
    )?;

    let (video_tx, mut video_rx) = mpsc::channel::<EncodedFrame>(64);
    let (_keyframe_tx, keyframe_rx) = mpsc::channel::<()>(1);
    let _encoder = AbortOnDropHandle::new(tokio::spawn(encode_h264_to_channel(
        comp.subscribe_frames(),
        video_tx,
        backend.clone(),
        Bitrate::from_kbps(cfg.graphics.bitrate),
        keyframe_rx,
    )));

    while let Some(frame) = video_rx.recv().await {
        if frame.config.is_some() {
            return Ok(frame);
        }
    }
    Err(eyre!("the encoder produced no keyframe"))
}

#[tokio::test]
#[ignore = "needs the GPU; run explicitly with --ignored"]
async fn the_config_frame_matches_the_iphones() -> Result<()> {
    let keyframe = first_keyframe().await?;
    let clock = MediaClockSession::new();

    let config = AvccConfigExtended {
        hevc: true,
        avcc: keyframe.config.clone().expect("keyframe carries its config"),
        video_latency: std::time::Duration::ZERO,
        width: 800,
        height: 1280,
        respect_timestamps: true,
        // The rect this head unit's panel asks for, so the view parameters are covered too.
        view: Some(VideoView {
            origin_x: 0.0,
            origin_y: 224.0,
            width: 800.0,
            height: 1056.0,
        }),
    };
    let frame = ScreenFrame::config(&config, std::time::Instant::now(), &clock);
    let entry = &frame.data[..];

    assert_eq!(frame.header.opcode, ScreenOpCode::VideoConfig);
    assert_eq!(
        frame.header.small_param[1..],
        [0x1e, 0x01],
        "RespectTimestamps | Encrypted | UseFormatDescription | Bit4 | NoDisplaySleep"
    );

    assert_eq!(&entry[4..8], b"hvc1");
    assert_eq!(&entry[14..16], &[0xff, 0xff], "dataRefIndex");
    assert_eq!(&entry[24..32], &[0, 0, 2, 0, 0, 0, 2, 0], "temporal/spatial quality");
    assert_eq!(&entry[32..36], &[0x03, 0x20, 0x05, 0x00], "800x1280");
    assert_eq!(&entry[50..55], b"\x04HEVC");
    assert_eq!(&entry[82..86], &[0, 0x18, 0xff, 0xff], "depth and clut id");

    let hvcc = entry
        .windows(4)
        .position(|w| w == b"hvcC")
        .expect("an hvcC atom")
        + 4;
    assert_eq!(entry[hvcc], 1, "configurationVersion");
    assert_ne!(entry[hvcc + 1], 0, "general_profile_idc");
    assert_ne!(
        &entry[hvcc + 2..hvcc + 6],
        &[0, 0, 0, 0],
        "general_profile_compatibility_flags"
    );
    assert_ne!(entry[hvcc + 12], 0, "general_level_idc");
    assert_eq!(entry[hvcc + 21] & 0x03, 3, "4-byte NAL length prefixes");

    println!("sample entry ({} bytes): {:02x?}", entry.len(), &entry[..96]);
    Ok(())
}

#[tokio::test]
#[ignore = "needs the GPU; run explicitly with --ignored"]
async fn a_keyframe_sample_opens_on_its_slice() -> Result<()> {
    let keyframe = first_keyframe().await?;
    let clock = MediaClockSession::new();

    let frame = EncodedVideoFrame {
        pts: Pts(std::time::Instant::now()),
        width: 800,
        height: 1280,
        data: bytes::BytesMut::from(&keyframe.data[..]),
        config: None,
        nal_offsets: None,
        is_keyframe: Some(true),
        length_prefixed: true,
        header_buf: bytes::BytesMut::new(),
        chacha_tag_buf: bytes::BytesMut::new(),
    };
    let sample = ScreenFrame::video_proxied(frame, 4, &clock).expect("frame converts");

    assert_eq!(
        &sample.data[..],
        &keyframe.data[..],
        "an already length-prefixed sample goes out untouched"
    );

    let headers = nal_headers(&sample.data).expect("length prefixes describe the sample exactly");
    assert!(!headers.is_empty());
    assert!(
        HevcNalType::from_byte(headers[0]).is_irap(),
        "a receiver reads the first NAL to decide the stream can open here, got {:?}",
        HevcNalType::from_byte(headers[0])
    );
    assert!(
        headers
            .iter()
            .all(|&h| ((h >> 1) & 0x3f) < 32),
        "samples carry coded slices and nothing else, got {:?}",
        headers.iter().map(|h| (h >> 1) & 0x3f).collect::<Vec<_>>()
    );
    Ok(())
}
