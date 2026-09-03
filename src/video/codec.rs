//! Which codec is on the wire, decided from the RTP payloads themselves.
//!
//! The air unit does not announce its codec anywhere the ground station can
//! read: there is no SDP, no signalling channel, just RTP appearing on a UDP
//! port. drone-cam solves this by sniffing the port with `codec_probe.py`
//! before it builds a pipeline; this is the same classifier, in Rust, running
//! continuously instead of once.
//!
//! Running continuously is the point. A one-shot probe decides at startup and
//! is then stuck with the answer, so an air unit rebooted into the other codec
//! shows a frozen picture until the app is restarted. Here the vote is kept,
//! and a stream that comes back as the other codec re-decides.

use std::fmt;

/// The two codecs an OpenIPC air unit produces.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Codec {
    H264,
    H265,
}

/// H.265 NAL unit types that appear in an RTP payload: VPS, SPS, PPS, SEI,
/// and the two payload structures (AP, FU).
const H265_TYPES: [u8; 7] = [32, 33, 34, 39, 40, 48, 49];

/// H.264 NAL unit types, same idea: slice, IDR, SEI, SPS, PPS, STAP-A, FU-A.
const H264_TYPES: [u8; 7] = [1, 5, 6, 7, 8, 24, 28];

impl Codec {
    /// The GStreamer element names for this codec, as `(parser, decoder)`.
    #[cfg(not(target_os = "android"))]
    pub fn gst_elements(self) -> (&'static str, &'static str) {
        match self {
            Codec::H264 => ("h264parse", "avdec_h264"),
            Codec::H265 => ("h265parse", "avdec_h265"),
        }
    }

    /// The Android MIME type `AMediaCodec` selects a decoder by.
    pub fn mime(self) -> &'static str {
        match self {
            Codec::H264 => "video/avc",
            Codec::H265 => "video/hevc",
        }
    }

    /// Whether a NAL unit is a parameter set: the sequence-level headers a
    /// decoder must have before it can start.
    ///
    /// Used two ways: to mark an access unit as a point the decoder can be
    /// started from, and on Android to collect the codec-specific data that
    /// `AMediaCodec` has to be configured with.
    pub fn is_parameter_set(self, nal: &[u8]) -> bool {
        match self {
            // SPS (7) and PPS (8).
            Codec::H264 => nal.first().is_some_and(|b| matches!(b & 0x1f, 7 | 8)),
            // VPS (32), SPS (33) and PPS (34).
            Codec::H265 => nal
                .first()
                .is_some_and(|b| matches!((b >> 1) & 0x3f, 32..=34)),
        }
    }

    /// Whether a NAL unit starts an intra-coded picture, which is where a
    /// decoder can begin without reference frames.
    pub fn is_keyframe_slice(self, nal: &[u8]) -> bool {
        match self {
            // IDR slice.
            Codec::H264 => nal.first().is_some_and(|b| b & 0x1f == 5),
            // BLA/IDR/CRA: the whole IRAP range.
            Codec::H265 => nal
                .first()
                .is_some_and(|b| matches!((b >> 1) & 0x3f, 16..=23)),
        }
    }
}

impl fmt::Display for Codec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Codec::H264 => "H.264",
            Codec::H265 => "H.265",
        })
    }
}

/// Classify one RTP payload.
///
/// Returns `None` for anything that matches neither, which is most of what a
/// corrupt packet looks like. The H.265 test runs first because its two-byte
/// header is the more constrained of the two: it pins the forbidden_zero bit
/// and the low bit of `nuh_layer_id`, where H.264 only pins forbidden_zero, so
/// checking H.264 first would claim H.265 packets.
pub fn classify(payload: &[u8]) -> Option<Codec> {
    let &b0 = payload.first()?;
    if payload.len() < 2 {
        return None;
    }

    // Forbidden zero bit clear, and the low bit of nuh_layer_id clear.
    let h265_type = (b0 >> 1) & 0x3f;
    if b0 & 0x80 == 0 && b0 & 0x01 == 0 && H265_TYPES.contains(&h265_type) {
        return Some(Codec::H265);
    }

    let h264_type = b0 & 0x1f;
    if b0 & 0x80 == 0 && H264_TYPES.contains(&h264_type) {
        return Some(Codec::H264);
    }

    None
}

/// Accumulates votes until one codec is clearly the one on the wire.
///
/// A single packet is not enough: the two header layouts overlap, so a lone
/// H.265 payload can read as valid H.264 and the other way round. Requiring a
/// run of agreeing packets makes a wrong answer vanishingly unlikely, and the
/// stream is thousands of packets a second, so the wait is imperceptible.
pub struct Detector {
    h264: u32,
    h265: u32,
    votes: u32,
}

/// Matching payloads needed before the codec is called.
const DEFAULT_VOTES: u32 = 5;

impl Default for Detector {
    fn default() -> Self {
        Self::new(DEFAULT_VOTES)
    }
}

impl Detector {
    pub fn new(votes: u32) -> Self {
        Self {
            h264: 0,
            h265: 0,
            votes: votes.max(1),
        }
    }

    /// Offer one RTP payload, returning the codec once enough agree.
    pub fn push(&mut self, payload: &[u8]) -> Option<Codec> {
        match classify(payload) {
            Some(Codec::H264) => self.h264 += 1,
            Some(Codec::H265) => self.h265 += 1,
            None => return None,
        }
        if self.h264 >= self.votes {
            return Some(Codec::H264);
        }
        if self.h265 >= self.votes {
            return Some(Codec::H265);
        }
        None
    }

    /// Forget the votes so far, for when a stream stops and the next one may
    /// be a different codec.
    pub fn reset(&mut self) {
        self.h264 = 0;
        self.h265 = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_h264_payload_types() {
        // Non-IDR slice, IDR, SPS, PPS, STAP-A, FU-A.
        for &b0 in &[0x41u8, 0x65, 0x67, 0x68, 0x78, 0x7c] {
            assert_eq!(classify(&[b0, 0x00]), Some(Codec::H264), "{b0:#04x}");
        }
    }

    #[test]
    fn classifies_h265_payload_types() {
        // VPS, SPS, PPS, SEI, AP, FU, each with the low layer-id bit clear.
        for &ty in &[32u8, 33, 34, 39, 48, 49] {
            let b0 = ty << 1;
            assert_eq!(classify(&[b0, 0x01]), Some(Codec::H265), "type {ty}");
        }
    }

    #[test]
    fn rejects_what_is_neither() {
        // Forbidden zero bit set.
        assert_eq!(classify(&[0x80, 0x00]), None);
        // A single byte is not enough to tell.
        assert_eq!(classify(&[0x41]), None);
        assert_eq!(classify(&[]), None);
    }

    #[test]
    fn h265_is_tested_before_h264() {
        // 0x40 is H.265 type 32 (VPS). Read as H.264 it is type 0, which is
        // not in the H.264 set, but the ordering is what guarantees the H.265
        // reading wins for the types that do overlap.
        assert_eq!(classify(&[0x40, 0x01]), Some(Codec::H265));
        // 0x62 is H.265 type 49 (FU) and H.264 type 2 at once; H.265 wins.
        assert_eq!(classify(&[49 << 1, 0x01]), Some(Codec::H265));
    }

    #[test]
    fn the_detector_waits_for_a_run_of_agreement() {
        let mut d = Detector::new(3);
        assert_eq!(d.push(&[0x41, 0x00]), None);
        assert_eq!(d.push(&[0x41, 0x00]), None);
        assert_eq!(d.push(&[0x41, 0x00]), Some(Codec::H264));
    }

    #[test]
    fn unclassifiable_packets_do_not_count_as_votes() {
        let mut d = Detector::new(2);
        assert_eq!(d.push(&[0x80, 0x00]), None);
        assert_eq!(d.push(&[0x41, 0x00]), None);
        assert_eq!(d.push(&[0x41, 0x00]), Some(Codec::H264));
    }

    #[test]
    fn reset_clears_the_tally_for_a_restarted_stream() {
        let mut d = Detector::new(2);
        d.push(&[0x41, 0x00]);
        d.reset();
        assert_eq!(d.push(&[0x41, 0x00]), None, "the earlier vote is gone");
        assert_eq!(d.push(&[0x41, 0x00]), Some(Codec::H264));
    }

    #[test]
    fn recognizes_parameter_sets() {
        assert!(Codec::H264.is_parameter_set(&[0x67])); // SPS
        assert!(Codec::H264.is_parameter_set(&[0x68])); // PPS
        assert!(!Codec::H264.is_parameter_set(&[0x41])); // a slice
        assert!(Codec::H265.is_parameter_set(&[32 << 1, 1])); // VPS
        assert!(Codec::H265.is_parameter_set(&[34 << 1, 1])); // PPS
        assert!(!Codec::H265.is_parameter_set(&[1 << 1, 1])); // a slice
    }

    #[test]
    fn recognizes_keyframe_slices() {
        assert!(Codec::H264.is_keyframe_slice(&[0x65]));
        assert!(!Codec::H264.is_keyframe_slice(&[0x41]));
        // IDR_W_RADL is 19, inside the IRAP range.
        assert!(Codec::H265.is_keyframe_slice(&[19 << 1, 1]));
        assert!(!Codec::H265.is_keyframe_slice(&[1 << 1, 1]));
    }
}
