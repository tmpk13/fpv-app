// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! What the Link page shows, without the app: open the adapter, sit on a
//! channel, and print what arrives.
//!
//! For the question a black screen cannot answer - is the air unit
//! transmitting, is the channel right, is the link id right, is the key
//! right? Each of those stops the link at a different stage, and the four
//! counters below tell them apart:
//!
//! ```text
//! heard=0                     nothing on this channel at all
//! heard>0 ours=0              someone else's traffic; wrong link id
//! ours>0  session=none        wrong key
//! session=ok  out=0           the session is up but data will not open
//! out>0                       the link works; the rest is video
//! ```
//!
//! ```sh
//! cargo run --example link-probe -- --key gs.key --channel 161
//! ```

use std::time::{Duration, Instant};

use drone_app::radio::{Bandwidth, Radio, RadioConfig};
use drone_app::video::codec::Detector;
use drone_app::video::rtp::Depayloader;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        eprintln!(
            "usage: link-probe [--key PATH] [--channel N] [--bandwidth 20|40]\n\
             \x20                 [--link-id N] [--radio-port N] [--seconds N]"
        );
        return;
    }

    let opt = |name: &str| -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let num = |name: &str, default: u32| -> u32 {
        opt(name)
            .map(|v| {
                v.parse().unwrap_or_else(|_| {
                    eprintln!("{name}: \"{v}\" is not a number");
                    std::process::exit(2);
                })
            })
            .unwrap_or(default)
    };

    let key_path = opt("--key").unwrap_or_else(|| "gs.key".into());
    let key = std::fs::read(&key_path).unwrap_or_else(|err| {
        eprintln!("cannot read {key_path}: {err}");
        std::process::exit(1);
    });

    let config = RadioConfig {
        channel: num("--channel", 161) as u8,
        bandwidth: if num("--bandwidth", 20) >= 40 {
            Bandwidth::Mhz40
        } else {
            Bandwidth::Mhz20
        },
        link_id: num("--link-id", 7669206),
        radio_port: num("--radio-port", 0) as u8,
        key,
        vid: 0,
        pid: 0,
    };
    let seconds = num("--seconds", 15);

    println!(
        "key {key_path} ({} bytes), channel {} {}, link {} port {}",
        config.key.len(),
        config.channel,
        config.bandwidth.as_str(),
        config.link_id,
        config.radio_port
    );

    let mut radio = Radio::open(config, None).unwrap_or_else(|err| {
        eprintln!("cannot open the adapter: {err}");
        std::process::exit(1);
    });

    let deadline = Instant::now() + Duration::from_secs(u64::from(seconds));
    let mut packets = 0u64;
    let mut bytes = 0u64;
    let mut next_report = Instant::now();

    // The RTP side too, so the probe reports what the air unit is actually
    // sending rather than only that something is arriving. Access units per
    // second is the source frame rate, which is the number to compare a
    // decoder's output against - without it, "50 fps" has nothing to be
    // slow relative to.
    let mut detector = Detector::default();
    let mut depay: Option<Depayloader> = None;
    let mut units = 0u64;
    let mut keyframes = 0u64;
    let mut last_units = 0u64;

    while Instant::now() < deadline {
        // Draining the queue is what lets the counters move: a full queue
        // stops the link layer delivering, and would read as a dead link.
        //
        // Bounded by time rather than by the queue running dry. A working
        // link never runs dry - video arrives faster than a print loop -
        // so "drain until empty" never returns and the probe prints nothing
        // on exactly the links it is meant to confirm.
        let drain_until = Instant::now() + Duration::from_millis(200);
        while Instant::now() < drain_until {
            match radio.recv(Duration::from_millis(20)) {
                Some(packet) => {
                    packets += 1;
                    bytes += packet.len() as u64;

                    if depay.is_none() {
                        // The RTP header is 12 bytes plus four per CSRC
                        // entry; the detector only needs to land on the
                        // first NAL header.
                        let offset =
                            12 + 4 * usize::from(packet.first().copied().unwrap_or(0) & 0x0f);
                        if let Some(codec) = packet.get(offset..).and_then(|p| detector.push(p)) {
                            println!("codec: {codec}");
                            depay = Some(Depayloader::new(codec));
                        }
                    }
                    if let Some(unit) = depay.as_mut().and_then(|d| d.push(&packet)) {
                        units += 1;
                        keyframes += u64::from(unit.keyframe);
                    }
                }
                None => break,
            }
        }

        if Instant::now() >= next_report {
            next_report += Duration::from_secs(1);
            let stats = radio.stats();
            let link = &stats.link;
            let signal = stats
                .signal
                .rssi_dbm
                .map(|dbm| format!("{dbm} dBm"))
                .unwrap_or_else(|| "-".into());
            let rtp = depay.as_ref().map(|d| d.stats()).unwrap_or_default();
            println!(
                "{:<10} heard={:<7} ours={:<6} session={:<16} dec_err={:<4} \
                 out={:<7} fec={:<4} lost={:<4} | fps={:<4} rtp_lost={:<4} \
                 damaged={:<4} rssi={}",
                stats.chip,
                link.total_frames,
                link.frames,
                if link.has_session() {
                    format!("{} of {} ep{}", link.fec_k, link.fec_n, link.epoch)
                } else {
                    "none".into()
                },
                link.decrypt_errors,
                link.agg.packets_out,
                link.agg.recovered,
                link.agg.packets_lost,
                units - last_units,
                rtp.lost,
                rtp.damaged,
                signal,
            );
            last_units = units;
        }

        if let Some(fault) = radio.fault() {
            eprintln!("the adapter stopped: {fault}");
            break;
        }
    }

    println!(
        "\n{packets} packets, {bytes} bytes, {units} pictures ({keyframes} keyframes) in {seconds} s"
    );
    if units > 0 {
        println!(
            "source frame rate: {:.1} fps, {:.1} Mbit/s",
            units as f64 / f64::from(seconds),
            8.0 * bytes as f64 / f64::from(seconds) / 1e6
        );
    }
    let stats = radio.stats();
    let link = stats.link;
    if link.total_frames == 0 {
        println!("Nothing at all on this channel. Wrong channel, or nothing is transmitting.");
    } else if link.frames == 0 {
        println!("Traffic, but none of it ours. The link id does not match.");
    } else if !link.has_session() {
        println!("Our frames arrive but none open. The key is not the peer of the air unit's.");
    } else if link.agg.packets_out == 0 {
        println!("The session is up but no data opened. Two air units on one link id?");
    } else {
        println!("The link works.");
    }
}
