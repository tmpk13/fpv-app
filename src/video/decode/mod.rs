//! The one platform split in the video path.
//!
//! Both implementations take the same thing - Annex-B access units, one
//! picture at a time - and put frames into the same [`FrameSink`]. Everything
//! ahead of them ([`super::rtp`], [`super::codec`]) is shared, so the split is
//! exactly as wide as the decoding itself and no wider.
//!
//! - Desktop: GStreamer, fed through an `appsrc`. Not `udpsrc ! rtpdepay`:
//!   the RTP layer is ours, for the reasons in [`super::rtp`].
//! - Android: the NDK's `AMediaCodec`, which is hardware decode with no Java
//!   shim and no extra SDK.
//!
//! Both must be `Send`: they are built and driven from the receive thread.
//! GStreamer elements are, being GObjects. `AMediaCodec` is not, so that
//! implementation keeps the codec on a thread of its own and sends units to
//! it - see [`mediacodec`].

use super::{Codec, FrameSink};
use crate::video::rtp::AccessUnit;

#[cfg(not(target_os = "android"))]
mod gst;
#[cfg(target_os = "android")]
mod mediacodec;

/// A running decoder for one codec.
///
/// Frames come out through the [`FrameSink`] it was built with rather than as
/// a return value: both backends produce pictures asynchronously, on a
/// streaming thread of their own, so there is no call for a frame to be
/// returned from.
pub trait Decoder: Send {
    /// Hand over one access unit.
    ///
    /// Returning `Ok` means the unit was accepted, not that it decoded: a
    /// picture that fails later is counted through the sink instead.
    fn submit(&mut self, unit: &AccessUnit) -> Result<(), String>;
}

/// The longest run of shed pictures before one is converted regardless.
///
/// The floor under the frame rate, and the reason there is one: `behind` is
/// derived from a counter two threads keep between them, and the first
/// version of that counter was wrong in a way that made it permanently true.
/// The picture stopped completely - "packets are arriving but nothing is
/// decoding" - from an off-by-one. A rule that shedding cannot continue
/// forever turns that whole class of mistake into a slow display rather than
/// a blank one.
// Compiled on every platform so it can be tested on one that has no
// AMediaCodec; only the Android decoder calls it.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
const MAX_SHED_RUN: u32 = 8;

/// Whether to spend the colour conversion on this decoded picture.
///
/// Shedding is how the pipeline copes with a source faster than the CPU can
/// convert - 120 fps of 720p is more than a phone has - and it is safe here
/// because the decoder has already used the picture as a reference. What it
/// must not be able to do is shed everything.
///
/// Lives here rather than beside the Android decoder so it compiles, and is
/// tested, on a desktop - the same reason the colour conversion does.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn should_convert(behind: bool, shed_run: u32) -> bool {
    !behind || shed_run >= MAX_SHED_RUN
}

/// Build the decoder for this platform.
pub fn new(codec: Codec, sink: FrameSink) -> Result<Box<dyn Decoder>, String> {
    #[cfg(not(target_os = "android"))]
    {
        gst::GstDecoder::new(codec, sink).map(|d| Box::new(d) as Box<dyn Decoder>)
    }
    #[cfg(target_os = "android")]
    {
        mediacodec::MediaCodecDecoder::new(codec, sink).map(|d| Box::new(d) as Box<dyn Decoder>)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pipeline_that_is_keeping_up_converts_everything() {
        assert!(should_convert(false, 0));
        assert!(should_convert(false, 3));
    }

    #[test]
    fn falling_behind_sheds_the_picture() {
        assert!(!should_convert(true, 0));
        assert!(!should_convert(true, 1));
    }

    #[test]
    fn shedding_cannot_go_on_forever() {
        // The property that matters. However wrong the "behind" signal gets -
        // and it has been wrong, permanently true, which blanked the screen -
        // a picture gets through at least once every MAX_SHED_RUN.
        assert!(
            (0..=MAX_SHED_RUN).any(|run| should_convert(true, run)),
            "a decoder that always reports itself behind must still draw"
        );
        assert!(should_convert(true, MAX_SHED_RUN));
        assert!(should_convert(true, MAX_SHED_RUN + 1));
    }

    #[test]
    fn the_floor_still_leaves_most_of_the_shedding_intact() {
        // Shedding has to actually shed, or the conversion becomes the
        // bottleneck again and the input queue overflows - which is the bug
        // this whole mechanism exists to avoid.
        let forced = (0..100)
            .filter(|run| should_convert(true, *run % (MAX_SHED_RUN + 1)))
            .count();
        assert!(
            forced < 20,
            "{forced} of 100 forced through is not shedding"
        );
    }
}
