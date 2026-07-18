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
    video_tx: mpsc::Sender<Vec<u8>>,
) -> Result<()> {
    gst::init()?;

    // gop-size caps the keyframe interval (~1s at 30fps). Frequent keyframes keep startup
    // latency low - after dropping buffered frames we open the stream on the next keyframe,
    // so a short GOP bounds how long that takes.
    let encoder = std::env::var("CARPLAY_ENCODER")
        .unwrap_or_else(|_| String::from("nvh264enc gop-size=30"));
    let source = if std::env::var("CARPLAY_TEST_VIDEO").is_ok() {
        String::from(
            "videotestsrc is-live=true pattern=ball ! video/x-raw,width=800,height=480,framerate=30/1",
        )
    } else {
        format!("pipewiresrc path={node_id}")
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
                // The callback runs on a GStreamer streaming thread (not the tokio
                // runtime), so a blocking send is safe and backpressures the encoder.
                match tx.blocking_send(map.as_slice().to_vec()) {
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
