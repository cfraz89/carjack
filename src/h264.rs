use eyre::{Result, eyre};
use gst::prelude::*;
use tokio::time::{Duration, sleep};

pub async fn encode_h264(node_id: u32) -> Result<()> {
    gst::init().unwrap();

    // Create a pipeline string
    // pipewiresrc: captures the node
    // videoconvert: ensures the format matches what the GPU wants
    // nvh264enc: NVIDIA hardware encoder (change to vaapih264enc for Intel/AMD)
    let pipeline_str = format!(
        "pipewiresrc path={} ! \
        video/x-raw(memory:DMABuf) ! \
        cudaupload ! \
        cudaconvert ! \
        nvh264enc ! \
        h264parse ! \
        mp4mux ! \
        filesink location=output.mp4",
        // "pipewiresrc path={} ! \
        // video/x-raw(memory:DMABuf) ! \
        //      vaapipostproc ! \
        //      vaapih264enc ! \
        //      h264parse ! \
        //      mp4mux ! \
        //      filesink location=output.mp4",
        node_id
    );

    let pipeline = gst::parse::launch(&pipeline_str)?;
    pipeline.set_state(gst::State::Playing)?;
    sleep(Duration::from_secs(5)).await;

    pipeline.send_event(gst::event::Eos::new());

    // Wait for the EOS to traverse the pipeline
    let bus = pipeline.bus().unwrap();
    for msg in bus.iter_timed(gst::ClockTime::NONE) {
        use gst::MessageView;
        match msg.view() {
            MessageView::Eos(..) => break,

            MessageView::Error(err) => {
                eprintln!("Error: {}", err.error());
                break;
            }
            _ => (),
        }
    }

    pipeline.set_state(gst::State::Null)?;
    Ok(())
}
