// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! The wfb-ng link layer: 802.11 frames off the air in, video packets out.
//!
//! This is an independent implementation of the protocol wfb-ng speaks, not a
//! port of wfb-ng. It exists because wfb-ng is GPL-3.0 and devourer - which
//! the frames arrive through - is GPL-2.0 with no upgrade clause, and those
//! two cannot be combined. The wire format is documented in wfb-ng's own
//! headers and is reproduced here; the code is this project's.
//!
//! Nothing in this module knows about USB, radios or video. It takes byte
//! slices that happen to be 802.11 frames and produces byte slices that
//! happen to be RTP, which is what makes the whole link testable on a desktop
//! with no hardware at all - and what lets the same code run behind a real
//! adapter on a phone.
//!
//! ```text
//!  802.11 frame
//!       |
//!       +-- not "WB", or another link id  -> ignored
//!       |
//!   payload
//!       |
//!       +-- type 2, session: crypto_box open with gs.key
//!       |        -> session key, FEC k and n  -> new Aggregator
//!       |
//!       +-- type 1, data: ChaCha20-Poly1305 open with the session key
//!                -> (block, fragment) -> Aggregator -> RTP packet
//! ```

pub mod agg;
pub mod crypto;
pub mod fec;
pub mod frame;

use agg::{AggStats, Aggregator};
use crypto::{KeyPair, SessionCipher};
use fec::Fec;

pub use frame::{channel_id, split_channel_id};

/// Payload type of a data packet.
const PACKET_DATA: u8 = 0x1;
/// Payload type of a session announcement.
const PACKET_SESSION: u8 = 0x2;
/// The only erasure code the protocol defines.
const FEC_VDM_RS: u8 = 0x1;

/// Bytes of the header on a data packet: the type and the 64-bit nonce.
const BLOCK_HEADER_LEN: usize = 9;

/// Fixed part of a session announcement: epoch, channel id, FEC type, k, n,
/// then the session key. Anything after it is optional TLV attributes, which
/// this receiver does not need and steps over.
const SESSION_BODY_LEN: usize = 8 + 4 + 1 + 1 + 1 + crypto::SESSION_KEY_LEN;

/// What the link saw, for the Link page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LinkStats {
    /// Every frame the adapter delivered, ours or not.
    ///
    /// Worth counting precisely because most of it is not ours: a channel
    /// that is busy with other networks and a channel with nothing on it are
    /// the same picture, and they mean opposite things. Traffic with none of
    /// it ours is a link id that does not match; no traffic at all is the
    /// wrong channel, or an antenna problem.
    pub total_frames: u64,
    /// Frames on our channel id.
    pub frames: u64,
    /// Frames the adapter delivered with a failed checksum.
    pub crc_errors: u64,
    /// Frames whose payload was not a shape the protocol defines.
    pub malformed: u64,
    /// Packets that decrypted, and packets that did not. A steady stream of
    /// failures with frames arriving means the wrong key, which is otherwise
    /// indistinguishable from an air unit that is not transmitting.
    pub decrypted: u64,
    pub decrypt_errors: u64,
    /// Data packets that arrived before any session key did. Normal for the
    /// first second after power-up, a fault after that.
    pub awaiting_session: u64,
    /// Session announcements seen, and the number that actually started a
    /// new session. The air unit repeats itself once a second, so the first
    /// number climbs steadily and the second one should not.
    pub session_packets: u64,
    pub sessions: u64,
    /// The current session's parameters, zero before one exists.
    pub epoch: u64,
    pub fec_k: u8,
    pub fec_n: u8,
    /// Reassembly and erasure coding.
    pub agg: AggStats,
}

impl LinkStats {
    /// Whether a session key has been received and video can be decrypted.
    pub fn has_session(&self) -> bool {
        self.fec_k > 0
    }

    /// Loss the erasure code repaired, as a share of the packets it carried.
    /// The headline number for how hard the link is working.
    pub fn recovery_pct(&self) -> f64 {
        let total = self.agg.packets_out + self.agg.packets_lost;
        if total == 0 {
            return 0.0;
        }
        100.0 * self.agg.recovered as f64 / total as f64
    }

    /// Packets that never arrived, as a share of those that should have.
    pub fn loss_pct(&self) -> f64 {
        let total = self.agg.packets_out + self.agg.packets_lost;
        if total == 0 {
            return 0.0;
        }
        100.0 * self.agg.packets_lost as f64 / total as f64
    }
}

/// One session: the key the air unit announced and the block ring under it.
struct Session {
    cipher: SessionCipher,
    agg: Aggregator,
}

/// A ground station's receive side for one channel id.
pub struct Link {
    channel_id: u32,
    keys: KeyPair,
    session: Option<Session>,
    epoch: u64,
    stats: LinkStats,
}

impl Link {
    pub fn new(channel_id: u32, keys: KeyPair) -> Self {
        Self {
            channel_id,
            keys,
            session: None,
            epoch: 0,
            stats: LinkStats::default(),
        }
    }

    pub fn stats(&self) -> LinkStats {
        let mut stats = self.stats;
        if let Some(session) = self.session.as_ref() {
            stats.agg = session.agg.stats();
        }
        stats
    }

    pub fn channel_id(&self) -> u32 {
        self.channel_id
    }

    /// Note a frame the adapter delivered with a bad checksum.
    ///
    /// Counted rather than hidden: on a marginal link this rises long before
    /// the picture does anything visible, which makes it the earliest warning
    /// the ground station has.
    pub fn note_crc_error(&mut self) {
        self.stats.crc_errors += 1;
    }

    /// Feed one 802.11 frame, including its trailing checksum.
    ///
    /// `out` receives the packets the air unit put in, in order. Frames that
    /// are not ours cost one comparison and are not counted.
    pub fn push_frame(&mut self, frame: &[u8], out: &mut dyn FnMut(&[u8])) {
        self.stats.total_frames += 1;
        let Some(payload) = frame::payload(frame, self.channel_id) else {
            return;
        };
        self.stats.frames += 1;

        match payload[0] {
            PACKET_DATA => self.push_data(payload, out),
            PACKET_SESSION => self.push_session(payload),
            _ => self.stats.malformed += 1,
        }
    }

    /// A data packet: decrypt it and hand the fragment to reassembly.
    fn push_data(&mut self, payload: &[u8], out: &mut dyn FnMut(&[u8])) {
        if payload.len() < BLOCK_HEADER_LEN + crypto::AEAD_TAG_LEN + 3 {
            self.stats.malformed += 1;
            return;
        }
        let Some(session) = self.session.as_mut() else {
            self.stats.awaiting_session += 1;
            return;
        };

        let (header, sealed) = payload.split_at(BLOCK_HEADER_LEN);
        let mut nonce = [0u8; 8];
        nonce.copy_from_slice(&header[1..]);

        let plain = match session.cipher.open(&nonce, header, sealed) {
            Ok(plain) => plain,
            Err(_) => {
                self.stats.decrypt_errors += 1;
                return;
            }
        };
        self.stats.decrypted += 1;

        // The nonce is not just a nonce: it is where the fragment says which
        // block and which position within it it belongs to. Authenticating it
        // as additional data is what stops those being forged.
        let value = u64::from_be_bytes(nonce);
        session
            .agg
            .push(value >> 8, (value & 0xff) as usize, plain, out);
    }

    /// A session announcement: adopt the key and rebuild the block ring.
    fn push_session(&mut self, payload: &[u8]) {
        self.stats.session_packets += 1;

        let nonce_end = 1 + crypto::BOX_NONCE_LEN;
        if payload.len() < nonce_end + SESSION_BODY_LEN + crypto::BOX_MAC_LEN {
            self.stats.malformed += 1;
            return;
        }

        let body = match self
            .keys
            .open_session(&payload[1..nonce_end], &payload[nonce_end..])
        {
            Ok(body) => body,
            Err(_) => {
                self.stats.decrypt_errors += 1;
                return;
            }
        };
        if body.len() < SESSION_BODY_LEN {
            self.stats.malformed += 1;
            return;
        }
        self.stats.decrypted += 1;

        let epoch = u64::from_be_bytes(body[..8].try_into().expect("eight bytes"));
        let channel_id = u32::from_be_bytes(body[8..12].try_into().expect("four bytes"));
        let (fec_type, k, n) = (body[12], body[13] as usize, body[14] as usize);
        let mut key = [0u8; crypto::SESSION_KEY_LEN];
        key.copy_from_slice(&body[15..15 + crypto::SESSION_KEY_LEN]);

        // An announcement that opens with our key but describes someone
        // else's link, or a code we do not implement, is not usable.
        if epoch < self.epoch || channel_id != self.channel_id || fec_type != FEC_VDM_RS {
            self.stats.decrypt_errors += 1;
            return;
        }

        if self
            .session
            .as_ref()
            .is_some_and(|s| s.cipher.key() == &key)
        {
            // The same session, announced again. The air unit repeats this
            // every second, and rebuilding on each one would throw away every
            // block in flight once a second.
            return;
        }

        let Some(fec) = Fec::new(k, n) else {
            log::warn!("wfb: session announces an impossible code, k={k} n={n}");
            self.stats.malformed += 1;
            return;
        };

        log::info!("wfb: session epoch {epoch}, FEC {k} of {n}");
        self.epoch = epoch;
        self.stats.epoch = epoch;
        self.stats.fec_k = k as u8;
        self.stats.fec_n = n as u8;
        self.stats.sessions += 1;
        // The counts a previous session accumulated describe the same link
        // and are carried across, so a mid-flight rekey does not reset the
        // page the user is reading.
        let carried = self.session.as_ref().map(|s| s.agg.stats());
        let mut agg = Aggregator::new(fec);
        if let Some(carried) = carried {
            agg.restore(carried);
        }
        self.session = Some(Session {
            cipher: SessionCipher::new(key),
            agg,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vectors() -> serde_json::Value {
        let text = include_str!("../../tests/fixtures/wfb_vectors.json");
        serde_json::from_str(text).expect("the fixture file is valid JSON")
    }

    fn hex(value: &serde_json::Value) -> Vec<u8> {
        let text = value.as_str().expect("fixture field is a hex string");
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("valid hex"))
            .collect()
    }

    /// Wrap a wfb-ng payload in the 802.11 header the air unit sends it under.
    fn on_air(channel_id: u32, payload: &[u8]) -> Vec<u8> {
        let id = channel_id.to_be_bytes();
        let mut out = vec![0x08, 0x01, 0x00, 0x00];
        out.extend_from_slice(&[0xff; 6]);
        out.extend_from_slice(&[0x57, 0x42]);
        out.extend_from_slice(&id);
        out.extend_from_slice(&[0x57, 0x42]);
        out.extend_from_slice(&id);
        out.extend_from_slice(&[0x00, 0x00]);
        out.extend_from_slice(payload);
        out.extend_from_slice(&[0; 4]);
        out
    }

    fn link(v: &serde_json::Value) -> Link {
        let keys = KeyPair::from_bytes(&hex(&v["gs_key"])).unwrap();
        Link::new(v["channel_id"].as_u64().unwrap() as u32, keys)
    }

    /// The reference stream: one session announcement, then a whole block.
    #[test]
    fn a_reference_block_decodes_to_the_packets_that_went_in() {
        let v = vectors();
        let id = v["channel_id"].as_u64().unwrap() as u32;
        let mut link = link(&v);

        let mut got: Vec<Vec<u8>> = Vec::new();
        link.push_frame(&on_air(id, &hex(&v["session"]["wire"])), &mut |_| {
            panic!("a session packet carries no video")
        });
        assert!(link.stats().has_session());
        assert_eq!(link.stats().fec_k, 8);
        assert_eq!(link.stats().fec_n, 12);

        for packet in v["block"]["packets"].as_array().unwrap() {
            link.push_frame(&on_air(id, &hex(&packet["wire"])), &mut |p| {
                got.push(p.to_vec())
            });
        }

        let want: Vec<Vec<u8>> = v["block"]["payloads"]
            .as_array()
            .unwrap()
            .iter()
            .map(hex)
            .collect();
        assert_eq!(got, want);
        assert_eq!(link.stats().agg.packets_out, 8);
        assert_eq!(link.stats().decrypt_errors, 0);
    }

    #[test]
    fn the_reference_block_survives_losing_its_data_fragments() {
        let v = vectors();
        let id = v["channel_id"].as_u64().unwrap() as u32;
        let mut link = link(&v);
        link.push_frame(&on_air(id, &hex(&v["session"]["wire"])), &mut |_| {});

        let mut got: Vec<Vec<u8>> = Vec::new();
        for packet in v["block"]["packets"].as_array().unwrap() {
            // Four of the eight data fragments never arrive, which is exactly
            // what the four parity fragments can make up.
            if matches!(packet["fragment"].as_u64(), Some(1 | 3 | 4 | 6)) {
                continue;
            }
            link.push_frame(&on_air(id, &hex(&packet["wire"])), &mut |p| {
                got.push(p.to_vec())
            });
        }

        let want: Vec<Vec<u8>> = v["block"]["payloads"]
            .as_array()
            .unwrap()
            .iter()
            .map(hex)
            .collect();
        assert_eq!(got, want, "the erasure code must rebuild what was lost");
        assert_eq!(link.stats().agg.recovered, 4);
    }

    #[test]
    fn data_before_a_session_key_is_counted_rather_than_guessed_at() {
        let v = vectors();
        let id = v["channel_id"].as_u64().unwrap() as u32;
        let mut link = link(&v);

        for packet in v["block"]["packets"].as_array().unwrap() {
            link.push_frame(&on_air(id, &hex(&packet["wire"])), &mut |_| {
                panic!("nothing can decrypt without a session key")
            });
        }
        assert_eq!(link.stats().awaiting_session, 12);
        assert!(!link.stats().has_session());
    }

    #[test]
    fn the_wrong_key_file_reports_itself() {
        let v = vectors();
        let id = v["channel_id"].as_u64().unwrap() as u32;
        // A gs.key from a different pairing: the frames arrive, nothing opens.
        // Note this is not the same as swapping gs.key for drone.key, which
        // works fine - crypto_box is symmetric.
        let mut other = hex(&v["gs_key"]);
        other[0] ^= 0x40;
        let keys = KeyPair::from_bytes(&other).unwrap();
        let mut link = Link::new(id, keys);

        link.push_frame(&on_air(id, &hex(&v["session"]["wire"])), &mut |_| {});
        let stats = link.stats();
        assert_eq!(stats.frames, 1);
        assert_eq!(stats.session_packets, 1);
        assert_eq!(stats.decrypt_errors, 1);
        assert!(!stats.has_session());
    }

    #[test]
    fn a_repeated_announcement_does_not_restart_the_session() {
        let v = vectors();
        let id = v["channel_id"].as_u64().unwrap() as u32;
        let mut link = link(&v);
        let session = on_air(id, &hex(&v["session"]["wire"]));

        for _ in 0..5 {
            link.push_frame(&session, &mut |_| {});
        }
        assert_eq!(link.stats().session_packets, 5);
        assert_eq!(
            link.stats().sessions,
            1,
            "the air unit repeats itself once a second; that is not a rekey"
        );
    }

    #[test]
    fn a_session_for_another_link_is_refused() {
        let v = vectors();
        let id = v["channel_id"].as_u64().unwrap() as u32;
        let keys = KeyPair::from_bytes(&hex(&v["gs_key"])).unwrap();
        // Same keys, different channel id: a second air unit on the same
        // shared key, which must not take over this receiver's session.
        let mut link = Link::new(id + 1, keys);
        link.push_frame(&on_air(id + 1, &hex(&v["session"]["wire"])), &mut |_| {});
        assert!(!link.stats().has_session());
        assert_eq!(link.stats().decrypt_errors, 1);
    }

    #[test]
    fn traffic_from_other_networks_is_not_counted_at_all() {
        let v = vectors();
        let id = v["channel_id"].as_u64().unwrap() as u32;
        let mut link = link(&v);
        link.push_frame(&on_air(id ^ 0xff, &hex(&v["session"]["wire"])), &mut |_| {});
        assert_eq!(link.stats().frames, 0);
        assert_eq!(
            link.stats().total_frames,
            1,
            "it is still a frame that was heard, and the difference between \
             the two counts is what tells a wrong link id from a dead channel"
        );
    }

    #[test]
    fn a_truncated_packet_is_malformed_not_a_panic() {
        let v = vectors();
        let id = v["channel_id"].as_u64().unwrap() as u32;
        let mut link = link(&v);
        let full = hex(&v["session"]["wire"]);

        for len in 1..full.len() {
            link.push_frame(&on_air(id, &full[..len]), &mut |_| {});
        }
        let data = hex(&v["block"]["packets"][0]["wire"]);
        for len in 1..data.len() {
            link.push_frame(&on_air(id, &data[..len]), &mut |_| {});
        }
        assert!(!link.stats().has_session());
    }

    #[test]
    fn an_unknown_packet_type_is_counted_and_dropped() {
        let v = vectors();
        let id = v["channel_id"].as_u64().unwrap() as u32;
        let mut link = link(&v);
        link.push_frame(&on_air(id, &[0x7f, 1, 2, 3, 4]), &mut |_| {});
        assert_eq!(link.stats().malformed, 1);
        assert_eq!(link.stats().frames, 1);
    }

    #[test]
    fn a_flipped_bit_on_the_air_is_a_decrypt_error_not_a_bad_frame() {
        let v = vectors();
        let id = v["channel_id"].as_u64().unwrap() as u32;
        let mut link = link(&v);
        link.push_frame(&on_air(id, &hex(&v["session"]["wire"])), &mut |_| {});

        let mut damaged = hex(&v["block"]["packets"][0]["wire"]);
        let last = damaged.len() - 1;
        damaged[last] ^= 0x80;
        link.push_frame(&on_air(id, &damaged), &mut |_| {
            panic!("a damaged packet must not reach the video path")
        });
        assert_eq!(link.stats().decrypt_errors, 1);
    }
}
