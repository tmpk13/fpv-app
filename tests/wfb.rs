// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! The wfb-ng link layer against the reference implementations.
//!
//! `src/wfb/` is this project's own implementation of a protocol wfb-ng
//! defines, so a round trip through itself would prove only that it is
//! self-consistent. These vectors come from elsewhere: the ciphers from
//! libsodium, the erasure code from wfb-ng's own `fec.c`, both driven by
//! `tools/gen_wfb_fixtures.py`. If this file passes, the two implementations
//! agree byte for byte.

use drone_app::wfb::crypto::KeyPair;
use drone_app::wfb::fec::Fec;
use drone_app::wfb::Link;
use serde_json::Value;

fn vectors() -> Value {
    let text = include_str!("fixtures/wfb_vectors.json");
    serde_json::from_str(text).expect("the fixture file is valid JSON")
}

fn hex(value: &Value) -> Vec<u8> {
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

/// The parity bytes must be the ones wfb-ng's own encoder produces. Nothing
/// else in the stack can catch a wrong generator matrix: a self-consistent
/// implementation encodes and decodes its own output perfectly and drops
/// every real block on the floor.
#[test]
fn the_erasure_code_produces_wfb_ngs_parity_bytes() {
    for case in vectors()["fec"].as_array().unwrap() {
        let k = case["k"].as_u64().unwrap() as usize;
        let n = case["n"].as_u64().unwrap() as usize;
        let size = case["size"].as_u64().unwrap() as usize;

        let data: Vec<Vec<u8>> = case["data"].as_array().unwrap().iter().map(hex).collect();
        let want: Vec<Vec<u8>> = case["parity"].as_array().unwrap().iter().map(hex).collect();

        let fec = Fec::new(k, n).expect("the fixture parameters are valid");
        let refs: Vec<&[u8]> = data.iter().map(|d| d.as_slice()).collect();
        assert_eq!(fec.encode(&refs, size), want, "({k},{n}) parity");
    }
}

/// And the decode side has to invert the reference encoder's output, not just
/// its own.
#[test]
fn every_recoverable_loss_pattern_of_the_reference_block_is_recovered() {
    for case in vectors()["fec"].as_array().unwrap() {
        let k = case["k"].as_u64().unwrap() as usize;
        let n = case["n"].as_u64().unwrap() as usize;
        let size = case["size"].as_u64().unwrap() as usize;
        if k < 2 {
            continue;
        }

        let data: Vec<Vec<u8>> = case["data"].as_array().unwrap().iter().map(hex).collect();
        let parity: Vec<Vec<u8>> = case["parity"].as_array().unwrap().iter().map(hex).collect();
        let fec = Fec::new(k, n).unwrap();

        // Lose each data fragment in turn, and then a run of as many as the
        // parity can cover.
        let mut patterns: Vec<Vec<usize>> = (0..k).map(|i| vec![i]).collect();
        patterns.push((0..(n - k).min(k)).collect());
        patterns.push((k - (n - k).min(k)..k).collect());

        for lost in patterns {
            let mut have: Vec<&[u8]> = Vec::new();
            let mut index: Vec<usize> = Vec::new();
            let mut spare = k;
            for (i, fragment) in data.iter().enumerate() {
                if lost.contains(&i) {
                    have.push(&parity[spare - k]);
                    index.push(spare);
                    spare += 1;
                } else {
                    have.push(fragment);
                    index.push(i);
                }
            }
            let got = fec.decode(&have, &index, size).unwrap();
            for (out, &i) in got.iter().zip(&lost) {
                assert_eq!(out, &data[i], "({k},{n}) losing {lost:?}");
            }
        }
    }
}

/// The whole path, from a frame as it comes off the air to the packet the
/// video decoder is handed.
#[test]
fn a_reference_stream_arrives_intact() {
    let v = vectors();
    let id = v["channel_id"].as_u64().unwrap() as u32;
    let keys = KeyPair::from_bytes(&hex(&v["gs_key"])).unwrap();
    let mut link = Link::new(id, keys);

    let mut got: Vec<Vec<u8>> = Vec::new();
    link.push_frame(&on_air(id, &hex(&v["session"]["wire"])), &mut |_| {});
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

    let stats = link.stats();
    assert_eq!(stats.decrypt_errors, 0);
    assert_eq!(stats.agg.packets_lost, 0);
    assert_eq!(stats.agg.corrupt, 0);
}

/// Every subset of the reference block that should be enough, is enough. This
/// is the property that decides whether a real link with 30% loss shows video
/// or a black screen, and it is cheap to check exhaustively at (8,12).
#[test]
fn any_eight_of_the_twelve_fragments_reconstruct_the_block() {
    let v = vectors();
    let id = v["channel_id"].as_u64().unwrap() as u32;
    let packets: Vec<Vec<u8>> = v["block"]["packets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| hex(&p["wire"]))
        .collect();
    let want: Vec<Vec<u8>> = v["block"]["payloads"]
        .as_array()
        .unwrap()
        .iter()
        .map(hex)
        .collect();

    let n = packets.len();
    let k = want.len();
    assert_eq!((k, n), (8, 12));

    for mask in 0u32..(1 << n) {
        if mask.count_ones() as usize != k {
            continue;
        }
        let keys = KeyPair::from_bytes(&hex(&v["gs_key"])).unwrap();
        let mut link = Link::new(id, keys);
        link.push_frame(&on_air(id, &hex(&v["session"]["wire"])), &mut |_| {});

        let mut got: Vec<Vec<u8>> = Vec::new();
        for (i, packet) in packets.iter().enumerate() {
            if mask & (1 << i) == 0 {
                continue;
            }
            link.push_frame(&on_air(id, packet), &mut |p| got.push(p.to_vec()));
        }
        assert_eq!(got, want, "fragments {mask:012b}");
    }
}

/// A frame that is not ours must cost nothing but a comparison, whatever it
/// contains. This is the one path that runs on every frame of a busy channel.
#[test]
fn arbitrary_air_traffic_never_panics() {
    let v = vectors();
    let id = v["channel_id"].as_u64().unwrap() as u32;
    let keys = KeyPair::from_bytes(&hex(&v["gs_key"])).unwrap();
    let mut link = Link::new(id, keys);
    link.push_frame(&on_air(id, &hex(&v["session"]["wire"])), &mut |_| {});

    // A cheap deterministic generator: enough shapes to hit every length and
    // first-byte combination the parsers branch on.
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };

    for _ in 0..20_000 {
        let len = (next() % 200) as usize;
        let mut frame: Vec<u8> = (0..len).map(|_| (next() & 0xff) as u8).collect();
        // Half of them are addressed to us, so the payload parsers are
        // actually reached rather than filtered out at the first branch.
        if len >= 16 && next() & 1 == 0 {
            frame[10] = 0x57;
            frame[11] = 0x42;
            frame[12..16].copy_from_slice(&id.to_be_bytes());
        }
        link.push_frame(&frame, &mut |_| {});
    }
}
