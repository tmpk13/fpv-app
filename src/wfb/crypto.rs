// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! The two ciphers wfb-ng uses, and the key file that seeds them.
//!
//! Session packets are NaCl `crypto_box`: X25519 to agree a key from the
//! ground station's secret and the air unit's public key, then
//! XSalsa20-Poly1305. They carry the symmetric session key.
//!
//! Data packets are ChaCha20-Poly1305 under that session key - the *original*
//! construction with a 64-bit nonce, not the IETF one with 96. The difference
//! is not cosmetic: the nonce is a different length and the authenticated data
//! is laid out differently, so an IETF implementation rejects every packet.
//! This is the one place where reaching for the obvious crate would have
//! produced something that compiles, runs, and never decodes a frame.

use chacha20::cipher::{KeyIvInit, StreamCipher};
use chacha20::ChaCha20Legacy;
use poly1305::universal_hash::KeyInit;
use poly1305::Poly1305;
use subtle::ConstantTimeEq;

/// Bytes of a ChaCha20-Poly1305 session key.
pub const SESSION_KEY_LEN: usize = 32;
/// Bytes of the Poly1305 tag on a data packet.
pub const AEAD_TAG_LEN: usize = 16;
/// Bytes of the nonce on a session packet, and of a `crypto_box` nonce.
pub const BOX_NONCE_LEN: usize = 24;
/// Bytes the `crypto_box` MAC adds to a session packet.
pub const BOX_MAC_LEN: usize = 16;
/// Bytes of an X25519 key, public or secret.
pub const KEY_LEN: usize = 32;

/// Anything that stops a packet being read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CryptoError {
    /// The packet is too short to hold what its type says it holds.
    Truncated,
    /// The tag did not verify: a corrupt packet, or the wrong key.
    BadTag,
}

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::Truncated => "packet too short",
            Self::BadTag => "authentication failed",
        };
        f.write_str(text)
    }
}

/// The ground station's half of a wfb-ng key pair.
///
/// `wfb_keygen` writes `gs.key` as the ground station's secret key followed by
/// the air unit's public key, 32 bytes each. `drone.key` is the mirror image,
/// which is why a swapped pair decrypts nothing at all rather than partly
/// working.
#[derive(Clone)]
pub struct KeyPair {
    secret: crypto_box::SecretKey,
    peer: crypto_box::PublicKey,
}

impl KeyPair {
    /// Read a key from the 64 bytes of a `gs.key` file.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 2 * KEY_LEN {
            return None;
        }
        let mut secret = [0u8; KEY_LEN];
        let mut peer = [0u8; KEY_LEN];
        secret.copy_from_slice(&bytes[..KEY_LEN]);
        peer.copy_from_slice(&bytes[KEY_LEN..2 * KEY_LEN]);
        Some(Self {
            secret: crypto_box::SecretKey::from_bytes(secret),
            peer: crypto_box::PublicKey::from(peer),
        })
    }

    /// Open a session packet body: everything after the one type byte and the
    /// 24-byte nonce.
    pub fn open_session(&self, nonce: &[u8], sealed: &[u8]) -> Result<Vec<u8>, CryptoError> {
        use crypto_box::aead::Aead;

        if nonce.len() != BOX_NONCE_LEN || sealed.len() < BOX_MAC_LEN {
            return Err(CryptoError::Truncated);
        }
        let boxed = crypto_box::SalsaBox::new(&self.peer, &self.secret);
        boxed
            .decrypt(nonce.into(), sealed)
            .map_err(|_| CryptoError::BadTag)
    }
}

impl std::fmt::Debug for KeyPair {
    /// Deliberately says nothing about the key material.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("KeyPair(..)")
    }
}

/// A session key, and the ChaCha20-Poly1305 decryption it does.
#[derive(Clone)]
pub struct SessionCipher {
    key: [u8; SESSION_KEY_LEN],
}

impl SessionCipher {
    pub fn new(key: [u8; SESSION_KEY_LEN]) -> Self {
        Self { key }
    }

    pub fn key(&self) -> &[u8; SESSION_KEY_LEN] {
        &self.key
    }

    /// Decrypt one data packet in place of allocating twice.
    ///
    /// `nonce` is the 8 bytes as they travel - the big-endian
    /// `(block_idx << 8) | fragment_idx` - and `aad` is the 9-byte block
    /// header those bytes came from, authenticated but not encrypted.
    pub fn open(&self, nonce: &[u8; 8], aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if sealed.len() < AEAD_TAG_LEN {
            return Err(CryptoError::Truncated);
        }
        let (ciphertext, tag) = sealed.split_at(sealed.len() - AEAD_TAG_LEN);

        let mut cipher = ChaCha20Legacy::new(&self.key.into(), nonce.into());

        // The first keystream block is not payload: its leading 32 bytes are
        // the one-time Poly1305 key, and the rest is discarded. Running it
        // through the cipher also leaves the counter at 1, where the payload
        // starts.
        let mut block0 = [0u8; 64];
        cipher.apply_keystream(&mut block0);

        let mut poly_key = [0u8; 32];
        poly_key.copy_from_slice(&block0[..32]);

        // The authenticated message, laid out the way the original
        // construction wants it.
        //
        // `compute_unpadded` is the load-bearing call. Poly1305 pads a short
        // final block by appending a single 0x01 byte and treating the block
        // as partial; a full block instead carries an implicit 2^128 term.
        // The streaming `update_padded` pads with zeroes and counts the
        // result as a full block - right for GHASH, and right for the IETF
        // ChaCha20 construction whose own padding rules leave every block
        // full, and wrong here. This message is aad, length, ciphertext,
        // length with no padding between the parts, so it is almost never a
        // whole number of blocks. Getting it wrong compiles, runs, and
        // produces a tag that fails on every packet - looking exactly like
        // the wrong key.
        let mut message = Vec::with_capacity(aad.len() + ciphertext.len() + 16);
        message.extend_from_slice(aad);
        message.extend_from_slice(&(aad.len() as u64).to_le_bytes());
        message.extend_from_slice(ciphertext);
        message.extend_from_slice(&(ciphertext.len() as u64).to_le_bytes());
        let expected = Poly1305::new(&poly_key.into()).compute_unpadded(&message);

        if expected.ct_eq(tag).unwrap_u8() != 1 {
            return Err(CryptoError::BadTag);
        }

        let mut plain = ciphertext.to_vec();
        cipher.apply_keystream(&mut plain);
        Ok(plain)
    }
}

impl std::fmt::Debug for SessionCipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionCipher(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The vectors are generated from libsodium by `tools/gen_wfb_fixtures.py`
    /// and checked in, so this test needs neither libsodium nor a drone.
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

    #[test]
    fn data_packets_match_libsodium() {
        let v = vectors();
        let key: [u8; 32] = hex(&v["session_key"]).try_into().unwrap();
        let cipher = SessionCipher::new(key);

        for case in v["aead"].as_array().unwrap() {
            let nonce: [u8; 8] = hex(&case["nonce"]).try_into().unwrap();
            let aad = hex(&case["aad"]);
            let sealed = hex(&case["sealed"]);
            let plain = hex(&case["plain"]);
            assert_eq!(
                cipher.open(&nonce, &aad, &sealed),
                Ok(plain),
                "libsodium's ciphertext must open with our reading of it"
            );
        }
    }

    #[test]
    fn a_flipped_bit_anywhere_is_caught() {
        let v = vectors();
        let key: [u8; 32] = hex(&v["session_key"]).try_into().unwrap();
        let cipher = SessionCipher::new(key);
        let case = &v["aead"].as_array().unwrap()[0];
        let nonce: [u8; 8] = hex(&case["nonce"]).try_into().unwrap();
        let aad = hex(&case["aad"]);
        let sealed = hex(&case["sealed"]);

        for i in 0..sealed.len() {
            let mut damaged = sealed.clone();
            damaged[i] ^= 0x01;
            assert_eq!(
                cipher.open(&nonce, &aad, &damaged),
                Err(CryptoError::BadTag),
                "byte {i} of the packet went unnoticed"
            );
        }

        // The header is authenticated but not encrypted, so tampering there
        // has to be caught too - it is what carries the fragment number.
        let mut other_aad = aad.clone();
        other_aad[0] ^= 0x01;
        assert_eq!(
            cipher.open(&nonce, &other_aad, &sealed),
            Err(CryptoError::BadTag)
        );

        // And the nonce is not covered by the tag directly, but it keys the
        // stream, so a wrong one fails the same way.
        let mut other_nonce = nonce;
        other_nonce[7] ^= 0x01;
        assert_eq!(
            cipher.open(&other_nonce, &aad, &sealed),
            Err(CryptoError::BadTag)
        );
    }

    #[test]
    fn an_ietf_shaped_reading_would_not_have_worked() {
        // Guards the reason this module exists. The IETF construction pads
        // the additional data out to a 16-byte boundary before the length;
        // doing that here must fail, which is what proves the fixtures really
        // are the original construction rather than something either reading
        // would accept.
        let v = vectors();
        let key: [u8; 32] = hex(&v["session_key"]).try_into().unwrap();
        let cipher = SessionCipher::new(key);
        let case = &v["aead"].as_array().unwrap()[0];
        let nonce: [u8; 8] = hex(&case["nonce"]).try_into().unwrap();
        let mut padded_aad = hex(&case["aad"]);
        assert!(
            !padded_aad.len().is_multiple_of(16),
            "the fixture aad is a partial block"
        );
        padded_aad.resize(padded_aad.len().div_ceil(16) * 16, 0);
        assert_eq!(
            cipher.open(&nonce, &padded_aad, &hex(&case["sealed"])),
            Err(CryptoError::BadTag)
        );
    }

    #[test]
    fn session_packets_open_with_the_ground_station_key() {
        let v = vectors();
        let gs_key = hex(&v["gs_key"]);
        let keys = KeyPair::from_bytes(&gs_key).unwrap();
        let nonce = hex(&v["session"]["nonce"]);
        let sealed = hex(&v["session"]["sealed"]);
        assert_eq!(
            keys.open_session(&nonce, &sealed),
            Ok(hex(&v["session"]["plain"]))
        );
    }

    #[test]
    fn the_air_units_own_key_file_opens_the_same_packet() {
        let v = vectors();
        // Worth pinning down, because it is counterintuitive and it decides
        // what "wrong key" can mean. crypto_box agrees one shared secret from
        // either side of the pair, so drone.key - the same two keys the other
        // way round - opens exactly what gs.key opens. A swapped key file is
        // therefore NOT a failure mode; only a different key pair is.
        let keys = KeyPair::from_bytes(&hex(&v["drone_key"])).unwrap();
        assert_eq!(
            keys.open_session(&hex(&v["session"]["nonce"]), &hex(&v["session"]["sealed"])),
            Ok(hex(&v["session"]["plain"]))
        );
    }

    #[test]
    fn a_key_from_another_pairing_opens_nothing() {
        let v = vectors();
        // The real mistake: a gs.key generated for a different air unit. It
        // is a perfectly valid 64-byte file and every packet fails.
        let mut other = hex(&v["gs_key"]);
        other[0] ^= 0x40;
        let keys = KeyPair::from_bytes(&other).unwrap();
        assert_eq!(
            keys.open_session(&hex(&v["session"]["nonce"]), &hex(&v["session"]["sealed"])),
            Err(CryptoError::BadTag)
        );
    }

    #[test]
    fn a_short_key_file_is_rejected() {
        assert!(KeyPair::from_bytes(&[0u8; 63]).is_none());
        assert!(KeyPair::from_bytes(&[0u8; 64]).is_some());
    }

    #[test]
    fn short_packets_are_refused_before_any_crypto() {
        let cipher = SessionCipher::new([7u8; 32]);
        assert_eq!(
            cipher.open(&[0; 8], &[], &[0; 15]),
            Err(CryptoError::Truncated)
        );
    }
}
