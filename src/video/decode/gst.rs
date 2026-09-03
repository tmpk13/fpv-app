//! Desktop decode through GStreamer.
//!
//! The pipeline is deliberately short:
//!
//! ```text
//! appsrc ! h265parse ! avdec_h265 ! videoconvert ! appsink
//! ```
//!
//! drone-cam's `vrx.sh view` runs `udpsrc ! rtph265depay ! ... !
//! autovideosink`; the two ends are different here for the same reason. The
//! front is an `appsrc` because the RTP layer is ours (see [`crate::video`]),
//! and the back is an `appsink` because the frames have to reach an egui
//! texture rather than a window of GStreamer's own.
//!
//! Everything else about it is about latency. This is an FPV link, where a
//! picture that arrives late is worse than one that does not arrive at all, so
//! every buffering opportunity in the pipeline is turned off: the source is
//! live, the sink does not sync to a clock, and the queue between them is one
//! frame deep and drops rather than blocks.

use std::sync::Once;

use gstreamer::prelude::*;
use gstreamer_app::{AppSink, AppSinkCallbacks, AppSrc};
use gstreamer_video::prelude::VideoFrameExt;

use crate::video::rtp::AccessUnit;
use crate::video::{Codec, Frame, FrameSink};

use super::Decoder;

/// `gstreamer::init` is global and must run exactly once per process.
static INIT: Once = Once::new();

/// A GStreamer pipeline decoding one codec into a [`FrameSink`].
pub struct GstDecoder {
    pipeline: gstreamer::Pipeline,
    source: AppSrc,
}

impl GstDecoder {
    pub fn new(codec: Codec, sink: FrameSink) -> Result<Self, String> {
        INIT.call_once(|| {
            if let Err(err) = gstreamer::init() {
                log::error!("gstreamer init failed: {err}");
            }
        });

        let (parser, decoder) = codec.gst_elements();
        let media = match codec {
            Codec::H264 => "video/x-h264",
            Codec::H265 => "video/x-h265",
        };

        // Built from a description rather than element by element: it is the
        // same pipeline drone-cam runs, so keeping it readable as one line
        // means the two can be compared by eye.
        //
        // - `is-live=true` with `do-timestamp=true` stamps each unit as it
        //   arrives, which is what the link actually is. Trusting the RTP
        //   timestamps instead would need the sender's clock.
        // - `max-buffers=1 drop=true` on the sink is what bounds latency: a UI
        //   that falls behind loses frames instead of accumulating them.
        // - `sync=false` displays each frame as it decodes rather than holding
        //   it for its presentation time, which on a live feed is only delay.
        let description = format!(
            "appsrc name=src is-live=true do-timestamp=true format=time \
             caps={media},stream-format=byte-stream,alignment=au \
             ! {parser} ! {decoder} ! videoconvert \
             ! video/x-raw,format=RGBA \
             ! appsink name=sink sync=false max-buffers=1 drop=true"
        );

        let pipeline = gstreamer::parse::launch(&description)
            .map_err(|err| format!("building the pipeline: {err}"))?
            .downcast::<gstreamer::Pipeline>()
            .map_err(|_| "the parsed pipeline was not a Pipeline".to_string())?;

        let source = pipeline
            .by_name("src")
            .ok_or("no appsrc in the pipeline")?
            .downcast::<AppSrc>()
            .map_err(|_| "the src element was not an appsrc".to_string())?;

        let appsink = pipeline
            .by_name("sink")
            .ok_or("no appsink in the pipeline")?
            .downcast::<AppSink>()
            .map_err(|_| "the sink element was not an appsink".to_string())?;

        appsink.set_callbacks(
            AppSinkCallbacks::builder()
                .new_sample(move |appsink| {
                    match pull_frame(appsink) {
                        Ok(frame) => sink.put(frame),
                        Err(err) => {
                            log::warn!("dropping a frame: {err}");
                            sink.note_error();
                        }
                    }
                    Ok(gstreamer::FlowSuccess::Ok)
                })
                .build(),
        );

        watch_bus(&pipeline);

        pipeline
            .set_state(gstreamer::State::Playing)
            .map_err(|err| format!("starting the pipeline: {err}"))?;

        Ok(Self { pipeline, source })
    }
}

impl Decoder for GstDecoder {
    fn submit(&mut self, unit: &AccessUnit) -> Result<(), String> {
        // A damaged unit is still worth decoding. The decoder conceals what it
        // can, and on this link a picture with artifacts is far more useful
        // than a gap - dropping it would make a lossy link look like a frozen
        // one, which is the wrong diagnosis to hand the pilot.
        let buffer = gstreamer::Buffer::from_slice(unit.data.clone());
        self.source
            .push_buffer(buffer)
            .map(|_| ())
            .map_err(|err| format!("pushing to the pipeline: {err}"))
    }
}

impl Drop for GstDecoder {
    fn drop(&mut self) {
        // Without this the pipeline's threads outlive the decoder and keep
        // pushing into a sink whose receiver has gone.
        let _ = self.source.end_of_stream();
        if let Err(err) = self.pipeline.set_state(gstreamer::State::Null) {
            log::warn!("stopping the pipeline: {err}");
        }
    }
}

/// Pull one sample from the sink and copy it into a tightly packed frame.
fn pull_frame(appsink: &AppSink) -> Result<Frame, String> {
    let sample = appsink
        .pull_sample()
        .map_err(|err| format!("pulling a sample: {err}"))?;
    let caps = sample.caps().ok_or("a sample with no caps")?;
    let info = gstreamer_video::VideoInfo::from_caps(caps)
        .map_err(|err| format!("reading the video format: {err}"))?;
    let buffer = sample.buffer().ok_or("a sample with no buffer")?;

    let frame = gstreamer_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info)
        .map_err(|err| format!("mapping the frame: {err}"))?;

    let width = frame.width();
    let height = frame.height();
    let stride = frame.plane_stride()[0] as usize;
    let data = frame
        .plane_data(0)
        .map_err(|err| format!("plane 0: {err}"))?;

    // GStreamer pads rows out to an alignment, so the buffer is `stride` bytes
    // per row while egui wants exactly `width * 4`. Copying row by row is what
    // takes the padding out; a single memcpy of the whole plane would shear
    // the picture whenever stride and width disagree, which at 1920 they do
    // not and at most other widths they do.
    let row = width as usize * 4;
    let mut rgba = Vec::with_capacity(row * height as usize);
    for y in 0..height as usize {
        let start = y * stride;
        let end = start + row;
        if end > data.len() {
            return Err(format!("frame is short: {} < {end}", data.len()));
        }
        rgba.extend_from_slice(&data[start..end]);
    }

    Ok(Frame {
        width,
        height,
        rgba,
    })
}

/// Log pipeline errors and warnings in the background.
///
/// Without this a pipeline that fails - a missing decoder element, a stream it
/// cannot parse - goes quiet with no explanation anywhere, because GStreamer
/// reports those on the bus rather than from the call that started it.
///
/// A thread rather than `bus.add_watch`: that delivers messages through a glib
/// main loop, and this app runs eframe's event loop instead, so the watch
/// would be installed and never fire. Blocking on `iter_timed` needs no loop
/// at all. The thread ends by itself when the pipeline is dropped and the bus
/// with it.
fn watch_bus(pipeline: &gstreamer::Pipeline) {
    let Some(bus) = pipeline.bus() else {
        return;
    };
    let spawned = std::thread::Builder::new()
        .name("gst-bus".into())
        .spawn(move || {
            use gstreamer::MessageView;
            for message in bus.iter_timed(gstreamer::ClockTime::NONE) {
                match message.view() {
                    MessageView::Error(err) => log::error!(
                        "gstreamer: {} ({})",
                        err.error(),
                        err.debug().unwrap_or_default()
                    ),
                    MessageView::Warning(warning) => {
                        log::warn!("gstreamer: {}", warning.error())
                    }
                    MessageView::Eos(_) => return,
                    _ => {}
                }
            }
        });
    if let Err(err) = spawned {
        log::warn!("no gstreamer bus watch, errors will be silent: {err}");
    }
}
