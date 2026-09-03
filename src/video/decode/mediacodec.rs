//! Android decode through the NDK's `AMediaCodec`.
//!
//! This is hardware decode on every phone worth running the app on, and it
//! needs nothing but the NDK: `libmediandk.so` is a plain C API, so unlike
//! gps-gui-rs's BLE and location bridges there is no Java shim and no dex to
//! build. It is also why GStreamer is not used here - its Android build is a
//! separate SDK download with its own NDK pinning and static plugin
//! registration, to reach a decoder the platform already has.
//!
//! ## Why the codec lives on its own thread
//!
//! [`MediaCodec`] wraps a raw pointer and is neither `Send` nor `Sync`, but
//! [`super::Decoder`] is called from the receive thread. Rather than assert
//! something about the C API's thread safety that Android does not actually
//! promise, the codec never leaves the thread that created it: [`submit`]
//! sends access units down a channel and the thread owns everything else.
//!
//! [`submit`]: MediaCodecDecoder::submit
//!
//! ## Output format
//!
//! `AMediaCodec` reports its buffer layout only after the first frames are
//! in flight, through `INFO_OUTPUT_FORMAT_CHANGED`. So the picture size is
//! read from there rather than parsed out of the stream's SPS - the codec's
//! own answer is authoritative, and it carries the stride and slice height
//! that [`crate::video::yuv`] needs and an SPS does not have.

use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::thread;
use std::time::Duration;

use ndk::media::media_codec::{
    DequeuedInputBufferResult, DequeuedOutputBufferInfoResult, MediaCodec, MediaCodecDirection,
    MediaFormat,
};

use crate::video::rtp::AccessUnit;
use crate::video::yuv::{ColorSpace, Layout, PlaneLayout};
use crate::video::{yuv, Codec, Frame, FrameSink};

use super::Decoder;

/// Access units allowed to queue up for the codec thread.
///
/// Small on purpose. If the decoder cannot keep up, the useful response is to
/// throw pictures away and stay current, not to build a backlog that puts the
/// display further behind with every frame.
const QUEUE_DEPTH: usize = 3;

/// How long to wait for a codec buffer before going round the loop again.
///
/// Short enough that input and output keep taking turns on the one thread,
/// long enough not to spin: at 60 fps a frame is 16 ms, so this polls several
/// times per frame.
const BUFFER_TIMEOUT: Duration = Duration::from_millis(4);

/// Android's `MediaFormat` keys. Not exposed as constants by the `ndk` crate,
/// and stable platform API.
const KEY_MIME: &str = "mime";
const KEY_WIDTH: &str = "width";
const KEY_HEIGHT: &str = "height";
const KEY_STRIDE: &str = "stride";
const KEY_SLICE_HEIGHT: &str = "slice-height";
const KEY_COLOR_FORMAT: &str = "color-format";
const KEY_CROP_LEFT: &str = "crop-left";
const KEY_CROP_TOP: &str = "crop-top";
const KEY_CROP_RIGHT: &str = "crop-right";
const KEY_CROP_BOTTOM: &str = "crop-bottom";
/// Codec-specific data: the parameter sets, which a decoder needs before it
/// can be configured.
const KEY_CSD0: &str = "csd-0";

/// `MediaCodecInfo.CodecCapabilities` color formats.
const COLOR_FORMAT_YUV420_PLANAR: i32 = 19;
const COLOR_FORMAT_YUV420_SEMI_PLANAR: i32 = 21;
/// `COLOR_FormatYUV420Flexible`. A promise that the layout is some 4:2:0, with
/// the specific one only discoverable per buffer; in practice every device
/// that reports it delivers NV12.
const COLOR_FORMAT_YUV420_FLEXIBLE: i32 = 0x7f42_0888;
/// Qualcomm's `OMX_QCOM_COLOR_FormatYUV420PackedSemiPlanar32m`, which is NV12
/// with the alignment already described by stride and slice height.
const COLOR_FORMAT_QCOM_NV12: i32 = 0x7fa3_0c04;

/// The size a decoder is configured at before it reports its real one.
///
/// `AMediaCodec_configure` requires a width and height, but the true size only
/// arrives with the first output format change, so this is a placeholder that
/// every decoder accepts and none is held to.
const CONFIGURE_WIDTH: i32 = 1920;
const CONFIGURE_HEIGHT: i32 = 1080;

/// The receive thread's end of an Android hardware decoder.
pub struct MediaCodecDecoder {
    units: SyncSender<Vec<u8>>,
    /// Kept only to report units dropped before the codec saw them. Without
    /// it the one failure that matters here is the one nothing counts.
    sink: FrameSink,
}

impl MediaCodecDecoder {
    pub fn new(codec: Codec, sink: FrameSink) -> Result<Self, String> {
        let (tx, rx) = sync_channel(QUEUE_DEPTH);
        let reporter = sink.clone();

        thread::Builder::new()
            .name("video-decode".into())
            .spawn(move || {
                if let Err(err) = decode_loop(codec, &sink, rx) {
                    log::error!("android decoder stopped: {err}");
                    sink.note_error();
                }
            })
            .map_err(|err| format!("spawning the decoder thread: {err}"))?;

        Ok(Self {
            units: tx,
            sink: reporter,
        })
    }
}

impl Decoder for MediaCodecDecoder {
    fn submit(&mut self, unit: &AccessUnit) -> Result<(), String> {
        match self.units.try_send(unit.data.clone()) {
            Ok(()) => Ok(()),
            // The decoder is behind. Dropping the newest unit keeps the
            // receive thread moving - blocking it would turn a decode
            // backlog into packet loss - but it is not free: an inter-frame
            // codec cannot lose one picture in isolation, so this is counted
            // rather than swallowed.
            Err(TrySendError::Full(_)) => {
                self.sink.note_unit_dropped();
                Ok(())
            }
            Err(TrySendError::Disconnected(_)) => Err("the decoder thread has gone".into()),
        }
    }
}

/// Own the codec and pump it until the channel closes.
fn decode_loop(codec: Codec, sink: &FrameSink, units: Receiver<Vec<u8>>) -> Result<(), String> {
    // The first unit carries the parameter sets, because the caller only
    // starts submitting at a unit that has them (see `Session::feed`). The
    // codec is configured from it rather than from a later one, so that
    // `csd-0` is genuinely the stream's own headers.
    let first = units
        .recv()
        .map_err(|_| "the decoder was dropped before any data arrived".to_string())?;

    let decoder = MediaCodec::from_decoder_type(codec.mime())
        .ok_or_else(|| format!("no {} decoder on this device", codec.mime()))?;

    let mut format = MediaFormat::new();
    format.set_str(KEY_MIME, codec.mime());
    format.set_i32(KEY_WIDTH, CONFIGURE_WIDTH);
    format.set_i32(KEY_HEIGHT, CONFIGURE_HEIGHT);
    format.set_buffer(KEY_CSD0, &first);

    decoder
        .configure(&format, None, MediaCodecDirection::Decoder)
        .map_err(|err| format!("configuring the {} decoder: {err}", codec.mime()))?;
    decoder
        .start()
        .map_err(|err| format!("starting the decoder: {err}"))?;

    // Which decoder the platform picked would be the more useful line here -
    // it says whether this is hardware or the software fallback - but
    // `AMediaCodec_getName` is API 28, and the app's floor is 21. Not worth
    // raising the floor by seven years of devices for a log message.
    log::info!("android: decoding {codec} ({})", codec.mime());

    let mut layout: Option<PlaneLayout> = None;
    let mut rgba = Vec::new();
    // A monotonic presentation time. The codec only needs these to increase;
    // the display order is the arrival order on a live feed.
    let mut pts: u64 = 0;
    let mut pending = Some(first);
    let mut warned_format = false;

    loop {
        // Take the next unit without blocking, so output keeps draining even
        // when nothing is arriving.
        if pending.is_none() {
            match units.try_recv() {
                Ok(unit) => pending = Some(unit),
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
        }

        if let Some(unit) = pending.take() {
            match decoder.dequeue_input_buffer(BUFFER_TIMEOUT) {
                Ok(DequeuedInputBufferResult::Buffer(mut buffer)) => {
                    let target = buffer.buffer_mut();
                    if target.len() < unit.len() {
                        log::warn!(
                            "dropping a {}-byte unit: the codec offered {} bytes",
                            unit.len(),
                            target.len()
                        );
                        sink.note_error();
                    } else {
                        // The buffer is uninitialized memory, so this writes
                        // through MaybeUninit rather than copying into a slice
                        // of u8 that does not yet exist.
                        for (slot, &byte) in target.iter_mut().zip(unit.iter()) {
                            slot.write(byte);
                        }
                        let len = unit.len();
                        pts += 1;
                        if let Err(err) = decoder.queue_input_buffer(buffer, 0, len, pts, 0) {
                            log::warn!("queueing a unit: {err}");
                            sink.note_error();
                        }
                    }
                }
                // No input buffer free. Keep the unit and try again after the
                // output side has had a turn, which is what frees one.
                Ok(DequeuedInputBufferResult::TryAgainLater) => pending = Some(unit),
                Err(err) => return Err(format!("dequeueing an input buffer: {err}")),
            }
        }

        match decoder.dequeue_output_buffer(BUFFER_TIMEOUT) {
            Ok(DequeuedOutputBufferInfoResult::Buffer(output)) => {
                let info = *output.info();
                let produced = info.size() > 0;
                if produced {
                    match layout.as_ref() {
                        Some(layout) => {
                            let offset = info.offset().max(0) as usize;
                            let end = offset.saturating_add(info.size().max(0) as usize);
                            let data = output.buffer();
                            let slice = data.get(offset..end.min(data.len())).unwrap_or(&[]);
                            if yuv::to_rgba(slice, layout, &mut rgba) {
                                sink.put(Frame {
                                    width: layout.width,
                                    height: layout.height,
                                    rgba: std::mem::take(&mut rgba),
                                });
                            } else {
                                sink.note_error();
                            }
                        }
                        None => {
                            // A frame before any format change is not
                            // something the layout can be guessed for.
                            sink.note_error();
                        }
                    }
                }
                if let Err(err) = decoder.release_output_buffer(output, false) {
                    log::warn!("releasing an output buffer: {err}");
                }
            }
            Ok(DequeuedOutputBufferInfoResult::OutputFormatChanged) => {
                match read_layout(&decoder.output_format()) {
                    Ok(next) => {
                        log::info!(
                            "android: decoding {}x{} ({:?}, stride {})",
                            next.width,
                            next.height,
                            next.layout,
                            next.stride
                        );
                        layout = Some(next);
                    }
                    Err(err) => {
                        // Once, not per frame: an unsupported color format
                        // does not fix itself, and logging it at frame rate
                        // would bury everything else.
                        if !warned_format {
                            warned_format = true;
                            log::error!("android: unusable decoder output: {err}");
                        }
                        sink.note_error();
                    }
                }
            }
            // Deprecated since API 21 and never sent by a current platform,
            // but the enum still carries it.
            Ok(DequeuedOutputBufferInfoResult::OutputBuffersChanged) => {}
            Ok(DequeuedOutputBufferInfoResult::TryAgainLater) => {}
            Err(err) => return Err(format!("dequeueing an output buffer: {err}")),
        }
    }

    let _ = decoder.stop();
    Ok(())
}

/// Turn the codec's output format into a plane layout.
fn read_layout(format: &MediaFormat) -> Result<PlaneLayout, String> {
    let coded_width = format
        .i32(KEY_WIDTH)
        .ok_or("the output format has no width")?;
    let coded_height = format
        .i32(KEY_HEIGHT)
        .ok_or("the output format has no height")?;
    if coded_width <= 0 || coded_height <= 0 {
        return Err(format!("a {coded_width}x{coded_height} picture"));
    }

    let color = format
        .i32(KEY_COLOR_FORMAT)
        .ok_or("the output format has no color format")?;
    let layout = match color {
        COLOR_FORMAT_YUV420_PLANAR => Layout::I420,
        COLOR_FORMAT_YUV420_SEMI_PLANAR | COLOR_FORMAT_YUV420_FLEXIBLE | COLOR_FORMAT_QCOM_NV12 => {
            Layout::Nv12
        }
        other => return Err(format!("color format {other:#x} is not supported")),
    };

    // Both default to the coded size on devices that do not report them,
    // which is the unpadded case.
    let stride = format
        .i32(KEY_STRIDE)
        .filter(|s| *s >= coded_width)
        .unwrap_or(coded_width);
    let slice_height = format
        .i32(KEY_SLICE_HEIGHT)
        .filter(|s| *s >= coded_height)
        .unwrap_or(coded_height);

    // The crop rectangle is inclusive on both edges, so a 1920-wide picture
    // reports crop-right 1919. Off-by-one here costs a column of green.
    let crop_left = format.i32(KEY_CROP_LEFT).unwrap_or(0).max(0);
    let crop_top = format.i32(KEY_CROP_TOP).unwrap_or(0).max(0);
    let crop_right = format.i32(KEY_CROP_RIGHT).unwrap_or(coded_width - 1);
    let crop_bottom = format.i32(KEY_CROP_BOTTOM).unwrap_or(coded_height - 1);

    let width = (crop_right - crop_left + 1).clamp(1, coded_width);
    let height = (crop_bottom - crop_top + 1).clamp(1, coded_height);

    Ok(PlaneLayout {
        width: width as u32,
        height: height as u32,
        stride: stride as u32,
        slice_height: slice_height as u32,
        crop_x: crop_left as u32,
        crop_y: crop_top as u32,
        layout,
        color_space: ColorSpace::for_height(height as u32),
    })
}
