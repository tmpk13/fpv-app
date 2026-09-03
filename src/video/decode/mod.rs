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
