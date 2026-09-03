// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! The 802.11 layer: which frames are ours, and where their payload starts.
//!
//! wfb-ng does not associate with anything. It broadcasts data frames whose
//! two addresses are not addresses at all: "WB" (0x57 0x42) followed by a
//! 32-bit channel id. The receiver keeps the frames whose channel id matches
//! its own and ignores the rest of the air.
//!
//! 0x57 is worth a second look - its two low bits are the multicast and
//! locally-administered flags, so the whole thing is a well-formed locally
//! administered multicast MAC that no real card will ever claim.

/// The fixed 802.11 header wfb-ng transmits: frame control, duration, three
/// addresses and the sequence control field.
pub const HEADER_LEN: usize = 24;

/// The trailing frame check sequence. The Realtek receive path hands up the
/// whole frame including it, so it has to come off before the payload length
/// means anything.
pub const FCS_LEN: usize = 4;

/// The two bytes that start both of wfb-ng's addresses.
const SIGNATURE: [u8; 2] = [0x57, 0x42];

/// Offset of the second address, the one wfb-ng's own capture filter reads.
const ADDR2: usize = 10;

/// Compose the channel id an air unit and ground station must agree on.
///
/// The receiver filters on the whole 32 bits, so a wrong link id is
/// indistinguishable from an air unit that is switched off: every frame is
/// discarded before anything is decrypted, and no counter moves but the one
/// for frames that were not for us.
pub fn channel_id(link_id: u32, radio_port: u8) -> u32 {
    (link_id << 8) | u32::from(radio_port)
}

/// Split a channel id back into the link id and radio port it was made from.
pub fn split_channel_id(channel_id: u32) -> (u32, u8) {
    (channel_id >> 8, (channel_id & 0xff) as u8)
}

/// The channel id a frame carries, if it is a wfb-ng frame at all.
pub fn frame_channel_id(frame: &[u8]) -> Option<u32> {
    let addr2 = frame.get(ADDR2..ADDR2 + 6)?;
    if addr2[..2] != SIGNATURE {
        return None;
    }
    Some(u32::from_be_bytes([addr2[2], addr2[3], addr2[4], addr2[5]]))
}

/// The wfb-ng payload of a frame addressed to `channel_id`.
///
/// `None` covers every reason to ignore a frame: too short, not wfb-ng, or
/// someone else's link. They are deliberately not told apart - on a shared
/// channel the overwhelming majority of frames are simply other people's, and
/// counting them separately would only produce a number that scrolls.
pub fn payload(frame: &[u8], channel_id: u32) -> Option<&[u8]> {
    if frame_channel_id(frame)? != channel_id {
        return None;
    }
    let end = frame.len().checked_sub(FCS_LEN)?;
    frame.get(HEADER_LEN..end).filter(|p| !p.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame shaped like the ones wfb_tx sends.
    fn frame(channel_id: u32, payload: &[u8]) -> Vec<u8> {
        let id = channel_id.to_be_bytes();
        let mut out = vec![0x08, 0x01, 0x00, 0x00];
        out.extend_from_slice(&[0xff; 6]); // broadcast receiver
        out.extend_from_slice(&SIGNATURE);
        out.extend_from_slice(&id);
        out.extend_from_slice(&SIGNATURE);
        out.extend_from_slice(&id);
        out.extend_from_slice(&[0x00, 0x00]); // sequence control
        assert_eq!(out.len(), HEADER_LEN);
        out.extend_from_slice(payload);
        out.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]); // FCS
        out
    }

    #[test]
    fn the_channel_id_is_the_link_id_and_the_radio_port() {
        // The value drone-cam's config.sh defaults to.
        assert_eq!(channel_id(7669206, 0), 7669206 << 8);
        assert_eq!(channel_id(7669206, 1), (7669206 << 8) | 1);
        assert_eq!(split_channel_id(channel_id(7669206, 3)), (7669206, 3));
    }

    #[test]
    fn our_own_frames_give_up_their_payload() {
        let id = channel_id(7669206, 0);
        let f = frame(id, &[1, 2, 3, 4]);
        assert_eq!(payload(&f, id), Some(&[1u8, 2, 3, 4][..]));
    }

    #[test]
    fn the_fcs_is_not_part_of_the_payload() {
        let id = channel_id(1, 0);
        let f = frame(id, &[9; 100]);
        assert_eq!(payload(&f, id).unwrap().len(), 100);
    }

    #[test]
    fn another_link_id_on_the_same_channel_is_ignored() {
        let ours = channel_id(7669206, 0);
        let theirs = channel_id(7669207, 0);
        let f = frame(theirs, &[1, 2, 3]);
        assert_eq!(payload(&f, ours), None);
        // Same link, different radio port: also not ours.
        assert_eq!(payload(&frame(channel_id(7669206, 1), &[1]), ours), None);
    }

    #[test]
    fn ordinary_wifi_traffic_is_ignored() {
        // A beacon from a real access point: the address at offset 10 is a
        // vendor MAC, not "WB".
        let mut beacon = vec![0x80, 0x00, 0x00, 0x00];
        beacon.extend_from_slice(&[0xff; 6]);
        beacon.extend_from_slice(&[0x00, 0x1a, 0x2b, 0x3c, 0x4d, 0x5e]);
        beacon.extend_from_slice(&[0x00, 0x1a, 0x2b, 0x3c, 0x4d, 0x5e]);
        beacon.extend_from_slice(&[0x00, 0x00]);
        beacon.extend_from_slice(&[0; 64]);
        assert_eq!(frame_channel_id(&beacon), None);
        assert_eq!(payload(&beacon, channel_id(7669206, 0)), None);
    }

    #[test]
    fn a_runt_frame_does_not_panic() {
        let id = channel_id(1, 0);
        for len in 0..HEADER_LEN + FCS_LEN + 1 {
            let f = vec![0x57u8; len];
            let _ = payload(&f, id);
            let _ = frame_channel_id(&f);
        }
        // A header and an FCS with nothing between them is not a packet.
        let empty = frame(id, &[]);
        assert_eq!(payload(&empty, id), None);
    }
}
