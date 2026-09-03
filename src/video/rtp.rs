//! RTP receive and depayload: datagrams in, Annex-B access units out.
//!
//! This is the one piece of the video path that is the same on every target.
//! The decoders differ (GStreamer on desktop, `AMediaCodec` on Android) but
//! both take Annex-B, so everything up to that point is here and is ordinary
//! testable Rust.
//!
//! Doing it here rather than in GStreamer's `rtph265depay` is deliberate. The
//! link is a lossy 5.8 GHz broadcast, so *what the RTP layer saw* is the
//! interesting diagnostic - which is exactly what a depayloader inside a
//! pipeline throws away. Owning the sequence numbers is what lets the Link
//! page report loss at all, and it is the same number on the phone and on the
//! laptop because it is the same code.
//!
//! References: RFC 3550 (RTP), RFC 6184 (H.264 payload), RFC 7798 (H.265
//! payload).

use super::codec::Codec;

/// RTP fixed header length: version/flags, sequence, timestamp, SSRC.
const RTP_HEADER_LEN: usize = 12;

/// Annex-B start code. Four bytes rather than three throughout: decoders
/// accept either, and a fixed width keeps the offset arithmetic in
/// [`Depayloader::parameter_sets`] simple.
const START_CODE: [u8; 4] = [0, 0, 0, 1];

/// H.264 payload structures that are not a plain NAL unit (RFC 6184).
const H264_STAP_A: u8 = 24;
const H264_FU_A: u8 = 28;

/// H.265 payload structures that are not a plain NAL unit (RFC 7798).
const H265_AP: u8 = 48;
const H265_FU: u8 = 49;

/// A sequence jump larger than this is read as the stream restarting rather
/// than as that many lost packets.
///
/// Without it, one restart books tens of thousands of phantom losses and the
/// loss percentage never recovers. Half the sequence space is the usual
/// choice; this is far tighter because the link either loses a handful of
/// packets or drops out entirely, and a real gap of thousands would be a
/// multi-second outage that the "no video" timeout reports anyway.
const SEQ_RESET_THRESHOLD: u16 = 4096;

/// What the RTP layer saw, for the Link page.
///
/// Counted here rather than derived from the decoder's output because loss on
/// this link is normal and mostly invisible downstream: FEC in wfb-ng repairs
/// some of it, the decoder conceals more, and the picture can look fine while
/// the margin is nearly gone. These are the numbers that show that coming.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RtpStats {
    /// Datagrams accepted as RTP.
    pub packets: u64,
    /// Datagrams dropped before parsing: too short, or not RTP version 2.
    pub malformed: u64,
    /// Packets the sequence numbers say never arrived.
    pub lost: u64,
    /// Packets that arrived after a later one. Counted separately from loss
    /// because the two want opposite responses: reordering is harmless and
    /// self-correcting, loss is the link degrading.
    pub reordered: u64,
    /// Times the sequence number jumped far enough to read as a new stream.
    pub resets: u64,
    /// Payload bytes (RTP headers excluded), for the bitrate readout.
    pub bytes: u64,
    /// Access units handed to the decoder.
    pub access_units: u64,
    /// Access units assembled across a sequence gap, so likely to decode with
    /// artifacts or not at all.
    pub damaged: u64,
}

impl RtpStats {
    /// Share of expected packets that never arrived, as a percentage.
    ///
    /// The denominator is what *should* have arrived - the ones that did plus
    /// the ones the gaps account for - so a link losing half its packets
    /// reports 50%, not 100%.
    pub fn loss_pct(&self) -> f32 {
        let expected = self.packets + self.lost;
        if expected == 0 {
            return 0.0;
        }
        100.0 * self.lost as f32 / expected as f32
    }
}

/// One complete access unit: the Annex-B bytes of a single decodable picture.
#[derive(Clone, Debug, PartialEq)]
pub struct AccessUnit {
    /// Annex-B: each NAL unit preceded by a four-byte start code.
    pub data: Vec<u8>,
    /// The RTP timestamp the unit was assembled under, at the 90 kHz clock
    /// every H.264/H.265 payload format uses.
    pub timestamp: u32,
    /// A packet went missing inside this unit, so it is incomplete.
    pub damaged: bool,
    /// The unit carries a parameter set (SPS for H.264, VPS/SPS for H.265),
    /// which is what makes it a point the decoder can start from.
    pub keyframe: bool,
}

/// Reassembles RTP packets into access units for one codec.
///
/// Feed it datagrams with [`Depayloader::push`]; it returns a unit each time
/// one completes. It holds no socket and no clock, which is what makes the
/// whole thing testable from a byte slice.
pub struct Depayloader {
    codec: Codec,
    /// Annex-B bytes of the unit being assembled.
    au: Vec<u8>,
    /// Reassembly buffer for a fragmented NAL unit (FU-A / FU).
    fu: Vec<u8>,
    /// Whether `fu` holds a fragment run that has started but not ended.
    fu_active: bool,
    /// Sequence number of the last accepted packet, for gap detection.
    last_seq: Option<u16>,
    /// RTP timestamp of the unit being assembled. A change means the previous
    /// unit is finished.
    au_timestamp: Option<u32>,
    /// A gap was seen while the current unit was being assembled.
    au_damaged: bool,
    /// A parameter set was seen in the current unit.
    au_keyframe: bool,
    stats: RtpStats,
}

impl Depayloader {
    pub fn new(codec: Codec) -> Self {
        Self {
            codec,
            au: Vec::new(),
            fu: Vec::new(),
            fu_active: false,
            last_seq: None,
            au_timestamp: None,
            au_damaged: false,
            au_keyframe: false,
            stats: RtpStats::default(),
        }
    }

    pub fn codec(&self) -> Codec {
        self.codec
    }

    pub fn stats(&self) -> RtpStats {
        self.stats
    }

    /// Feed one UDP datagram.
    ///
    /// Returns the access unit that this packet *completed*, which is the one
    /// before it: a unit is only known to be finished once the next one starts
    /// (a new RTP timestamp) or the sender marks the end of a picture. So the
    /// unit comes out one packet later than it went in, which costs nothing
    /// here because that packet has already arrived.
    pub fn push(&mut self, datagram: &[u8]) -> Option<AccessUnit> {
        let Some((seq, timestamp, marker, payload)) = parse_header(datagram) else {
            self.stats.malformed += 1;
            return None;
        };

        self.track_sequence(seq);
        self.stats.packets += 1;
        self.stats.bytes += payload.len() as u64;

        // A new timestamp means a new picture, so whatever is buffered is
        // complete. Emit it before the packet that ended it is unpacked.
        let finished = match self.au_timestamp {
            Some(prev) if prev != timestamp => self.take_access_unit(),
            _ => None,
        };
        self.au_timestamp = Some(timestamp);

        match self.codec {
            Codec::H264 => self.push_h264(payload),
            Codec::H265 => self.push_h265(payload),
        }

        // The marker bit is the sender saying "last packet of this picture",
        // which finishes the unit without waiting for the next timestamp. Not
        // every encoder sets it, hence the timestamp check above as well.
        if marker {
            return finished.or_else(|| self.take_access_unit());
        }
        finished
    }

    /// Count a packet's sequence number against the previous one.
    fn track_sequence(&mut self, seq: u16) {
        let Some(last) = self.last_seq else {
            self.last_seq = Some(seq);
            return;
        };

        // Wrapping arithmetic is what makes the 16-bit rollover a non-event:
        // 65535 -> 0 is a delta of 1, not of -65535.
        let delta = seq.wrapping_sub(last);
        match delta {
            // The expected next packet.
            1 => {}
            // The same packet twice, or one that arrived late. Neither is
            // loss, and neither should move `last_seq` backwards.
            0 => self.stats.reordered += 1,
            d if d > SEQ_RESET_THRESHOLD => {
                // Either a huge forward jump or (as a wrapped negative) a
                // packet from well before the last one. Both read as the
                // stream having restarted rather than as loss.
                self.stats.resets += 1;
                self.abort_fragment();
                self.au_damaged = true;
            }
            d => {
                self.stats.lost += u64::from(d - 1);
                // A gap mid-picture means the unit being assembled is missing
                // a piece, and a fragment run spanning the gap cannot be
                // completed at all.
                self.abort_fragment();
                self.au_damaged = true;
            }
        }

        // Advance on everything except a repeat of the same number. A repeat
        // must not move it, or the packet after it looks like a gap; a genuine
        // late arrival is rare enough on this link that treating it as the new
        // position costs at most one phantom gap and keeps this simple.
        if delta != 0 {
            self.last_seq = Some(seq);
        }
    }

    /// Emit the buffered access unit, if there is one worth emitting.
    fn take_access_unit(&mut self) -> Option<AccessUnit> {
        // A fragment run left open when the picture ended is a truncated NAL
        // unit; dropping it is better than handing the decoder a partial one.
        self.abort_fragment();

        if self.au.is_empty() {
            self.au_damaged = false;
            self.au_keyframe = false;
            return None;
        }

        let unit = AccessUnit {
            data: std::mem::take(&mut self.au),
            timestamp: self.au_timestamp.unwrap_or(0),
            damaged: self.au_damaged,
            keyframe: self.au_keyframe,
        };
        self.stats.access_units += 1;
        if unit.damaged {
            self.stats.damaged += 1;
        }
        self.au_damaged = false;
        self.au_keyframe = false;
        Some(unit)
    }

    /// Discard a partially reassembled fragment run.
    fn abort_fragment(&mut self) {
        self.fu.clear();
        self.fu_active = false;
    }

    /// Append one complete NAL unit to the access unit under assembly.
    fn emit_nal(&mut self, nal: &[u8]) {
        if nal.is_empty() {
            return;
        }
        if self.codec.is_parameter_set(nal) {
            self.au_keyframe = true;
        }
        self.au.extend_from_slice(&START_CODE);
        self.au.extend_from_slice(nal);
    }

    /// RFC 6184: single NAL units, STAP-A aggregates, and FU-A fragments.
    fn push_h264(&mut self, payload: &[u8]) {
        let Some(&first) = payload.first() else {
            return;
        };
        match first & 0x1f {
            H264_STAP_A => {
                // One byte of STAP-A header, then (16-bit size, NAL) pairs.
                for nal in aggregated(&payload[1..]) {
                    self.emit_nal(nal);
                }
            }
            H264_FU_A => {
                // FU indicator, FU header, then the fragment.
                if payload.len() < 3 {
                    return;
                }
                let fu_header = payload[1];
                let start = fu_header & 0x80 != 0;
                let end = fu_header & 0x40 != 0;
                // The original NAL header is the indicator's F and NRI bits
                // with the type taken from the FU header.
                let nal_header = (payload[0] & 0xe0) | (fu_header & 0x1f);
                self.push_fragment(start, end, &[nal_header], &payload[2..]);
            }
            // Types 1..=23 are a NAL unit sent whole. The reserved and
            // undefined values (0, 25..=27, 29..=31) fall here too and are
            // passed through: a decoder ignores what it does not know, and
            // dropping them silently would hide a real stream from the user.
            _ => self.emit_nal(payload),
        }
    }

    /// RFC 7798: the same three shapes, with a two-byte NAL header.
    fn push_h265(&mut self, payload: &[u8]) {
        if payload.len() < 2 {
            return;
        }
        match (payload[0] >> 1) & 0x3f {
            H265_AP => {
                // Two bytes of AP header, then (16-bit size, NAL) pairs. No
                // DONL: wfb-ng's senders do not use the sprop-max-don-diff
                // mode that would add one, and assuming it when it is absent
                // would eat the first two bytes of every aggregated unit.
                for nal in aggregated(&payload[2..]) {
                    self.emit_nal(nal);
                }
            }
            H265_FU => {
                // Two-byte payload header, one-byte FU header, then the
                // fragment.
                if payload.len() < 4 {
                    return;
                }
                let fu_header = payload[2];
                let start = fu_header & 0x80 != 0;
                let end = fu_header & 0x40 != 0;
                // Rebuild the original two-byte header: keep the layer id and
                // temporal id, put the fragment's type back in place of 49.
                let nal_type = fu_header & 0x3f;
                let header = [(payload[0] & 0x81) | (nal_type << 1), payload[1]];
                self.push_fragment(start, end, &header, &payload[3..]);
            }
            _ => self.emit_nal(payload),
        }
    }

    /// Accumulate one fragment of a NAL unit split across packets.
    ///
    /// `header` is the reconstructed NAL header, written once at the start of
    /// the run; `body` is this packet's slice of the unit.
    fn push_fragment(&mut self, start: bool, end: bool, header: &[u8], body: &[u8]) {
        if start {
            // A new run replaces whatever was there: an unfinished previous
            // run means its end packet was lost, and the two must not be
            // concatenated into one corrupt NAL unit.
            self.fu.clear();
            self.fu.extend_from_slice(header);
            self.fu_active = true;
        } else if !self.fu_active {
            // A middle or end fragment with no start: the start packet was
            // lost, so the unit cannot be reassembled.
            return;
        }
        self.fu.extend_from_slice(body);

        if end {
            let nal = std::mem::take(&mut self.fu);
            self.fu_active = false;
            self.emit_nal(&nal);
        }
    }
}

/// Split the body of a STAP-A / AP aggregate into its NAL units.
///
/// Each is a 16-bit big-endian length followed by that many bytes. A truncated
/// tail ends the iteration rather than erroring: the aggregate came off a
/// lossy link, and the units before the damage are still good.
fn aggregated(mut body: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    while body.len() >= 2 {
        let len = usize::from(u16::from_be_bytes([body[0], body[1]]));
        let rest = &body[2..];
        if len == 0 || len > rest.len() {
            break;
        }
        out.push(&rest[..len]);
        body = &rest[len..];
    }
    out
}

/// Parse an RTP header, returning the sequence, timestamp, marker bit and
/// payload.
///
/// Returns `None` for anything that is not RTP version 2 or is too short to
/// hold its own declared header, which is what keeps a stray datagram on the
/// port from being decoded as video.
fn parse_header(packet: &[u8]) -> Option<(u16, u32, bool, &[u8])> {
    if packet.len() <= RTP_HEADER_LEN || packet[0] >> 6 != 2 {
        return None;
    }

    let csrc_count = usize::from(packet[0] & 0x0f);
    let has_extension = packet[0] & 0x10 != 0;
    let marker = packet[1] & 0x80 != 0;
    let seq = u16::from_be_bytes([packet[2], packet[3]]);
    let timestamp = u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]);

    let mut offset = RTP_HEADER_LEN + 4 * csrc_count;
    if offset > packet.len() {
        return None;
    }

    if has_extension {
        // Extension header: 16-bit profile, 16-bit length in 32-bit words.
        if offset + 4 > packet.len() {
            return None;
        }
        let words = usize::from(u16::from_be_bytes([packet[offset + 2], packet[offset + 3]]));
        offset += 4 + 4 * words;
        if offset > packet.len() {
            return None;
        }
    }

    // Padding: the last byte says how many trailing bytes to drop, itself
    // included. wfb-ng does not pad, but a stray padded packet would otherwise
    // feed the padding to the decoder as NAL bytes.
    let mut end = packet.len();
    if packet[0] & 0x20 != 0 {
        let pad = usize::from(packet[end - 1]);
        if pad == 0 || pad > end.saturating_sub(offset) {
            return None;
        }
        end -= pad;
    }

    if offset >= end {
        return None;
    }
    Some((seq, timestamp, marker, &packet[offset..end]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an RTP packet around a payload.
    fn packet(seq: u16, timestamp: u32, marker: bool, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0x80, if marker { 0x80 } else { 0x00 }];
        out.extend_from_slice(&seq.to_be_bytes());
        out.extend_from_slice(&timestamp.to_be_bytes());
        out.extend_from_slice(&0xdead_beefu32.to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    /// The Annex-B NAL units in an access unit, without their start codes.
    ///
    /// Splits on the start code itself rather than on zero bytes, so a NAL
    /// whose payload happens to contain a zero still comes back whole.
    fn nals(unit: &AccessUnit) -> Vec<Vec<u8>> {
        let data = &unit.data;
        let mut starts = Vec::new();
        let mut i = 0;
        while i + START_CODE.len() <= data.len() {
            if data[i..i + START_CODE.len()] == START_CODE {
                starts.push(i + START_CODE.len());
                i += START_CODE.len();
            } else {
                i += 1;
            }
        }
        starts
            .iter()
            .enumerate()
            .map(|(n, &begin)| {
                let end = starts
                    .get(n + 1)
                    .map_or(data.len(), |&next| next - START_CODE.len());
                data[begin..end].to_vec()
            })
            .collect()
    }

    #[test]
    fn parses_a_plain_header() {
        let p = packet(7, 900, false, &[0x41, 0xaa]);
        let (seq, ts, marker, payload) = parse_header(&p).unwrap();
        assert_eq!((seq, ts, marker), (7, 900, false));
        assert_eq!(payload, &[0x41, 0xaa]);
    }

    #[test]
    fn rejects_non_rtp_and_short_datagrams() {
        // Version 1 rather than 2.
        assert!(parse_header(&[0x40, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]).is_none());
        // Header only, no payload.
        assert!(parse_header(&packet(1, 0, false, &[])).is_none());
        assert!(parse_header(&[0x80, 0x60]).is_none());
    }

    #[test]
    fn skips_csrc_and_extension_headers() {
        // Two CSRC entries and an extension of one word ahead of the payload.
        let mut p = vec![0x80 | 0x10 | 2, 0x00];
        p.extend_from_slice(&1u16.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&[0; 8]); // two CSRCs
        p.extend_from_slice(&[0xbe, 0xde, 0x00, 0x01]); // extension, 1 word
        p.extend_from_slice(&[0; 4]);
        p.extend_from_slice(&[0x41, 0x99]);
        let (_, _, _, payload) = parse_header(&p).unwrap();
        assert_eq!(payload, &[0x41, 0x99]);
    }

    #[test]
    fn strips_padding() {
        let mut p = packet(1, 0, false, &[0x41, 0x99]);
        p[0] |= 0x20;
        p.extend_from_slice(&[0, 0, 3]); // three padding bytes, count included
        let (_, _, _, payload) = parse_header(&p).unwrap();
        assert_eq!(payload, &[0x41, 0x99]);
    }

    #[test]
    fn single_h264_nal_becomes_an_access_unit() {
        let mut d = Depayloader::new(Codec::H264);
        // A non-IDR slice, then a picture at a new timestamp to close it.
        assert!(d
            .push(&packet(1, 100, false, &[0x41, 0x01, 0x02]))
            .is_none());
        let unit = d.push(&packet(2, 200, false, &[0x41, 0x03])).unwrap();
        assert_eq!(nals(&unit), vec![vec![0x41, 0x01, 0x02]]);
        assert_eq!(unit.timestamp, 100);
        assert!(!unit.damaged);
    }

    #[test]
    fn marker_bit_closes_a_unit_without_the_next_packet() {
        let mut d = Depayloader::new(Codec::H264);
        let unit = d.push(&packet(1, 100, true, &[0x41, 0x01])).unwrap();
        assert_eq!(nals(&unit), vec![vec![0x41, 0x01]]);
        assert_eq!(d.stats().access_units, 1);
    }

    #[test]
    fn fu_a_reassembles_and_rebuilds_the_original_nal_header() {
        let mut d = Depayloader::new(Codec::H264);
        // Type 5 (IDR) with NRI 3, split into three fragments. The FU
        // indicator is F=0, NRI=3, type=28.
        let indicator = 0x7c;
        assert!(d
            .push(&packet(1, 100, false, &[indicator, 0x85, 0xaa]))
            .is_none());
        assert!(d
            .push(&packet(2, 100, false, &[indicator, 0x05, 0xbb]))
            .is_none());
        let unit = d
            .push(&packet(3, 100, true, &[indicator, 0x45, 0xcc]))
            .unwrap();
        // NRI 3 from the indicator, type 5 from the FU header: 0x65.
        assert_eq!(nals(&unit), vec![vec![0x65, 0xaa, 0xbb, 0xcc]]);
    }

    #[test]
    fn fu_a_without_its_start_fragment_is_dropped() {
        let mut d = Depayloader::new(Codec::H264);
        let indicator = 0x7c;
        // Middle and end only: the start packet never arrived.
        d.push(&packet(1, 100, false, &[indicator, 0x05, 0xbb]));
        let unit = d.push(&packet(2, 100, true, &[indicator, 0x45, 0xcc]));
        assert!(unit.is_none(), "a headless fragment run must not decode");
    }

    #[test]
    fn splits_an_h264_stap_a_aggregate() {
        let mut d = Depayloader::new(Codec::H264);
        // STAP-A carrying an SPS and a PPS, the usual pairing.
        let mut payload = vec![0x78]; // type 24
        payload.extend_from_slice(&2u16.to_be_bytes());
        payload.extend_from_slice(&[0x67, 0x42]); // SPS
        payload.extend_from_slice(&3u16.to_be_bytes());
        payload.extend_from_slice(&[0x68, 0xce, 0x3c]); // PPS
        let unit = d.push(&packet(1, 100, true, &payload)).unwrap();
        assert_eq!(nals(&unit), vec![vec![0x67, 0x42], vec![0x68, 0xce, 0x3c]]);
        assert!(unit.keyframe, "an SPS makes the unit a start point");
    }

    #[test]
    fn a_truncated_aggregate_keeps_the_units_before_the_damage() {
        let mut body = Vec::new();
        body.extend_from_slice(&2u16.to_be_bytes());
        body.extend_from_slice(&[0x67, 0x42]);
        body.extend_from_slice(&9u16.to_be_bytes()); // claims more than is left
        body.extend_from_slice(&[0x68]);
        assert_eq!(aggregated(&body), vec![&[0x67, 0x42][..]]);
    }

    #[test]
    fn reassembles_an_h265_fu_run() {
        let mut d = Depayloader::new(Codec::H265);
        // Payload header type 49, layer 0, tid 1; fragments of type 19 (IDR).
        let hdr = [49u8 << 1, 0x01];
        d.push(&packet(1, 100, false, &[hdr[0], hdr[1], 0x80 | 19, 0xaa]));
        d.push(&packet(2, 100, false, &[hdr[0], hdr[1], 19, 0xbb]));
        let unit = d
            .push(&packet(3, 100, true, &[hdr[0], hdr[1], 0x40 | 19, 0xcc]))
            .unwrap();
        // Type 19 back in the two-byte header, temporal id preserved.
        assert_eq!(nals(&unit), vec![vec![19 << 1, 0x01, 0xaa, 0xbb, 0xcc]]);
    }

    #[test]
    fn splits_an_h265_ap_aggregate() {
        let mut d = Depayloader::new(Codec::H265);
        let mut payload = vec![48u8 << 1, 0x01]; // type 48
        payload.extend_from_slice(&2u16.to_be_bytes());
        payload.extend_from_slice(&[32 << 1, 0x01]); // VPS
        payload.extend_from_slice(&2u16.to_be_bytes());
        payload.extend_from_slice(&[33 << 1, 0x01]); // SPS
        let unit = d.push(&packet(1, 100, true, &payload)).unwrap();
        assert_eq!(nals(&unit).len(), 2);
        assert!(unit.keyframe, "a VPS/SPS pair makes the unit a start point");
    }

    #[test]
    fn counts_a_sequence_gap_as_loss() {
        let mut d = Depayloader::new(Codec::H264);
        d.push(&packet(10, 100, true, &[0x41, 0x01]));
        // 11 and 12 never arrive.
        d.push(&packet(13, 200, true, &[0x41, 0x02]));
        let stats = d.stats();
        assert_eq!(stats.lost, 2);
        assert_eq!(stats.packets, 2);
        // Two of four expected packets arrived.
        assert!((stats.loss_pct() - 50.0).abs() < 0.01);
    }

    #[test]
    fn a_gap_marks_the_unit_it_lands_in_as_damaged() {
        let mut d = Depayloader::new(Codec::H264);
        d.push(&packet(1, 100, false, &[0x41, 0x01]));
        let unit = d.push(&packet(5, 200, true, &[0x41, 0x02])).unwrap();
        assert!(unit.damaged);
        assert_eq!(d.stats().damaged, 1);
    }

    #[test]
    fn sequence_wraparound_is_not_loss() {
        let mut d = Depayloader::new(Codec::H264);
        d.push(&packet(65535, 100, true, &[0x41, 0x01]));
        d.push(&packet(0, 200, true, &[0x41, 0x02]));
        assert_eq!(d.stats().lost, 0, "65535 -> 0 is the next packet");
        assert_eq!(d.stats().resets, 0);
    }

    #[test]
    fn a_large_jump_reads_as_a_restart_rather_than_loss() {
        let mut d = Depayloader::new(Codec::H264);
        d.push(&packet(10, 100, true, &[0x41, 0x01]));
        d.push(&packet(40000, 200, true, &[0x41, 0x02]));
        let stats = d.stats();
        assert_eq!(stats.resets, 1);
        assert_eq!(stats.lost, 0, "a restart must not book 40000 losses");
    }

    #[test]
    fn a_duplicate_packet_is_counted_but_does_not_rewind_the_sequence() {
        let mut d = Depayloader::new(Codec::H264);
        d.push(&packet(10, 100, true, &[0x41, 0x01]));
        d.push(&packet(10, 100, true, &[0x41, 0x01]));
        d.push(&packet(11, 200, true, &[0x41, 0x02]));
        let stats = d.stats();
        assert_eq!(stats.reordered, 1);
        assert_eq!(
            stats.lost, 0,
            "the duplicate must not make 11 look like a gap"
        );
    }

    #[test]
    fn a_malformed_datagram_is_counted_and_ignored() {
        let mut d = Depayloader::new(Codec::H264);
        assert!(d.push(&[0x00, 0x01, 0x02]).is_none());
        assert_eq!(d.stats().malformed, 1);
        assert_eq!(d.stats().packets, 0);
    }

    #[test]
    fn payload_bytes_are_counted_for_the_bitrate() {
        let mut d = Depayloader::new(Codec::H264);
        d.push(&packet(1, 100, true, &[0x41; 40]));
        d.push(&packet(2, 200, true, &[0x41; 60]));
        assert_eq!(d.stats().bytes, 100, "RTP headers are not stream bitrate");
    }
}
