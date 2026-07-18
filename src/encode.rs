use eyre::{Result, eyre};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use futures::StreamExt;
use tokio::sync::mpsc;

/// Run a GStreamer pipeline that H264-encodes the desktop (or a test pattern) and hands
/// each encoded access unit to `video_tx`.
///
/// Uses an in-process `appsink` rather than piping `gst-launch` through an fd: the sink
/// delivers one `alignment=au` buffer per callback, so each message on the channel is
/// already exactly one complete access unit - no byte-stream reframing needed.
pub async fn encode_h264_to_channel(
    node_id: u32,
    width: u32,
    height: u32,
    video_tx: mpsc::Sender<Vec<u8>>,
) -> Result<()> {
    gst::init()?;

    // gop-size caps the keyframe interval. After RECORD we drop buffered frames and open
    // the stream on the next keyframe, so this bounds startup latency - the car tears the
    // session down if it gets no video for ~1s. gop-size=6 => a keyframe every ~200ms.
    let encoder = std::env::var("CARPLAY_ENCODER")
        .unwrap_or_else(|_| String::from("nvh264enc gop-size=6"));
    // The output must be exactly the car's declared display size - CarPlay head units size
    // their decode framebuffer to it and reject a mismatched stream. videoscale rescales the
    // captured desktop (or test pattern) to fit.
    let source = if std::env::var("CARPLAY_TEST_VIDEO").is_ok() {
        format!("videotestsrc is-live=true pattern=ball ! video/x-raw,width={width},height={height},framerate=30/1")
    } else {
        format!("pipewiresrc path={node_id} ! videoscale ! videorate ! video/x-raw,width={width},height={height},framerate=30/1")
    };

    let desc = format!(
        "{source} ! videoconvert ! {encoder} ! h264parse config-interval=-1 ! \
         video/x-h264,stream-format=byte-stream,alignment=au ! \
         appsink name=sink sync=false",
    );
    tracing::info!("Starting GStreamer pipeline: {desc}");

    let pipeline = gst::parse::launch(&desc)?
        .downcast::<gst::Pipeline>()
        .map_err(|_| eyre!("parsed element is not a pipeline"))?;

    let appsink = pipeline
        .by_name("sink")
        .ok_or_else(|| eyre!("appsink 'sink' not found"))?
        .downcast::<gst_app::AppSink>()
        .map_err(|_| eyre!("'sink' is not an appsink"))?;

    let tx = video_tx.clone();
    appsink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                // h264parse byte-stream uses a 4-byte start code for the first NAL of each
                // access unit but 3-byte ones for the rest. The AirPlay screen sink's NAL
                // parser only recognizes 4-byte start codes, so normalize every NAL to a
                // 4-byte prefix - otherwise it mis-parses the keyframe into one giant NAL
                // and the car drops the (undecodable) stream ~100ms in.
                let au = normalize_annexb_4byte(map.as_slice());
                // The callback runs on a GStreamer streaming thread (not the tokio
                // runtime), so a blocking send is safe and backpressures the encoder.
                match tx.blocking_send(au) {
                    Ok(()) => Ok(gst::FlowSuccess::Ok),
                    Err(_) => Err(gst::FlowError::Eos),
                }
            })
            .build(),
    );

    // Ensure the pipeline is torn down if this future is dropped (e.g. the session ends
    // and the surrounding select! picks another branch).
    struct StopGuard(gst::Pipeline);
    impl Drop for StopGuard {
        fn drop(&mut self) {
            let _ = self.0.set_state(gst::State::Null);
        }
    }

    pipeline.set_state(gst::State::Playing)?;
    let _guard = StopGuard(pipeline.clone());

    let bus = pipeline
        .bus()
        .ok_or_else(|| eyre!("pipeline has no bus"))?;
    let mut messages = bus.stream();
    while let Some(msg) = messages.next().await {
        match msg.view() {
            gst::MessageView::Eos(_) => {
                tracing::info!("GStreamer pipeline reached end of stream");
                break;
            }
            gst::MessageView::Error(err) => {
                return Err(eyre!(
                    "GStreamer error from {:?}: {}",
                    err.src().map(|s| s.path_string()),
                    err.error()
                ));
            }
            _ => {}
        }
    }

    Ok(())
}

/// Rewrite every NAL in an Annex-B access unit behind a 4-byte (`00 00 00 01`) start code.
/// gstreamer emits a mix of 3- and 4-byte prefixes; the AirPlay screen sink only accepts
/// 4-byte ones.
fn normalize_annexb_4byte(buf: &[u8]) -> Vec<u8> {
    // Offsets of every "00 00 01" start-code core.
    let mut cores: Vec<usize> = Vec::new();
    let mut i = 0;
    while i + 3 <= buf.len() {
        if buf[i] == 0 && buf[i + 1] == 0 && buf[i + 2] == 1 {
            cores.push(i);
            i += 3;
        } else {
            i += 1;
        }
    }
    if cores.is_empty() {
        return buf.to_vec();
    }

    // Start of a start code including any leading zero bytes, so a NAL's payload doesn't
    // absorb the following start code's extra 0x00.
    let true_start = |p: usize| -> usize {
        let mut s = p;
        while s > 0 && buf[s - 1] == 0 {
            s -= 1;
        }
        s
    };

    let mut out = Vec::with_capacity(buf.len() + cores.len());
    for k in 0..cores.len() {
        let payload_start = cores[k] + 3;
        let payload_end = if k + 1 < cores.len() {
            true_start(cores[k + 1])
        } else {
            buf.len()
        };
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&buf[payload_start..payload_end]);
    }
    out
}
