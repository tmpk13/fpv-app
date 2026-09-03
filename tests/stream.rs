//! End-to-end tests over a real RTP stream.
//!
//! The unit tests cover the RTP layer and the pixel math from byte slices.
//! What they cannot cover is whether the whole path fits together: whether a
//! real encoder's packets depayload into units a real decoder accepts, and
//! whether frames come out the other end at the right size. That needs an
//! actual stream, so these run `ffmpeg` and point it at the app's own
//! receiver: the same arrangement as `tools/fake-stream.sh`, and the same one
//! drone-cam's `wfb_rx -u` produces.
//!
//! No window is opened. `egui::Context` is pure CPU and can be built headless,
//! so the video path can be driven exactly as the app drives it with nothing
//! on screen.
//!
//! These are skipped rather than failed when ffmpeg is missing or has no
//! encoder, so a checkout without it still passes `cargo test`.

use std::net::Ipv4Addr;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use drone_app::video::{self, Codec, Source};

/// How long to wait for the first decoded frame.
///
/// Generous: it covers ffmpeg starting, the encoder's first keyframe, and the
/// decoder's own startup. The tests return as soon as the frame arrives, so
/// this is only the ceiling on a failure.
const DEADLINE: Duration = Duration::from_secs(20);

const WIDTH: u32 = 320;
const HEIGHT: u32 = 240;

/// A running ffmpeg, killed when the test ends however it ends.
struct Sender(Child);

impl Drop for Sender {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Whether ffmpeg is present with the encoder a codec needs.
fn encoder_available(encoder: &str) -> bool {
    let Ok(output) = Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .stderr(Stdio::null())
        .output()
    else {
        return false;
    };
    String::from_utf8_lossy(&output.stdout).contains(&format!(" {encoder} "))
}

/// Start ffmpeg sending a test pattern as RTP to `port`.
fn send(encoder: &str, port: u16) -> Sender {
    let child = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-re",
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc2=size={WIDTH}x{HEIGHT}:rate=30"),
            "-c:v",
            encoder,
            "-preset",
            "ultrafast",
            "-tune",
            "zerolatency",
            // A keyframe every 15 frames, so the receiver's wait for a
            // parameter set is half a second rather than several.
            "-g",
            "15",
            "-f",
            "rtp",
            &format!("udp://127.0.0.1:{port}"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("ffmpeg should start");
    Sender(child)
}

/// A port nothing else is using, taken by binding one and letting it go.
///
/// A fixed port would make two of these tests collide when cargo runs them in
/// parallel, and would collide with a real wfb_rx on the developer's machine.
fn free_port() -> u16 {
    let socket = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("binding a port");
    socket.local_addr().expect("the bound address").port()
}

/// Run one codec end to end and return the stats once a frame has decoded.
fn decode_one(codec: Codec, encoder: &str) -> Option<video::Stats> {
    if !encoder_available(encoder) {
        eprintln!("skipping: no {encoder} in this ffmpeg");
        return None;
    }

    let port = free_port();
    let handle = video::spawn(
        egui::Context::default(),
        Source {
            bind: Ipv4Addr::LOCALHOST,
            port,
            // Detection is what the app does by default, so it is what the
            // test exercises. `detects_the_codec_from_the_stream` below checks
            // the answer.
            codec: None,
        },
    );

    // Started after the receiver is listening, so the first keyframe is not
    // sent into a closed port.
    let _sender = send(encoder, port);

    let deadline = Instant::now() + DEADLINE;
    while Instant::now() < deadline {
        let stats = handle.stats();
        if stats.frames > 0 {
            return Some(stats);
        }
        if let Some(reason) = stats.bind_error {
            panic!("could not listen on {port}: {reason}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let stats = handle.stats();
    panic!(
        "no frame decoded for {codec} in {DEADLINE:?}: \
         {} packets, {} access units, {} decode errors",
        stats.rtp.packets, stats.rtp.access_units, stats.decode_errors
    );
}

#[test]
fn decodes_an_h264_stream_end_to_end() {
    let Some(stats) = decode_one(Codec::H264, "libx264") else {
        return;
    };
    assert_eq!(stats.codec, Some(Codec::H264));
    assert_eq!((stats.width, stats.height), (WIDTH, HEIGHT));
    assert!(stats.frames > 0);
}

#[test]
fn decodes_an_h265_stream_end_to_end() {
    let Some(stats) = decode_one(Codec::H265, "libx265") else {
        return;
    };
    assert_eq!(stats.codec, Some(Codec::H265));
    assert_eq!((stats.width, stats.height), (WIDTH, HEIGHT));
    assert!(stats.frames > 0);
}

#[test]
fn a_real_stream_arrives_without_loss_over_the_loopback() {
    let Some(stats) = decode_one(Codec::H264, "libx264") else {
        return;
    };
    // Nothing is dropped on the loopback, so any loss reported here would be
    // the sequence tracking miscounting rather than the network.
    assert_eq!(
        stats.rtp.lost, 0,
        "loopback lost nothing, so this is the sequence tracking: {:?}",
        stats.rtp
    );
    assert_eq!(stats.rtp.malformed, 0, "a real RTP packet was rejected");
    assert!(
        stats.rtp.packets > 0 && stats.bitrate_bps > 0.0,
        "the rate window reported nothing for a live stream"
    );
}

#[test]
fn a_port_with_nothing_on_it_reports_no_traffic_rather_than_failing() {
    let handle = video::spawn(
        egui::Context::default(),
        Source {
            bind: Ipv4Addr::LOCALHOST,
            port: free_port(),
            codec: None,
        },
    );
    std::thread::sleep(Duration::from_millis(600));
    let stats = handle.stats();
    assert!(stats.bind_error.is_none(), "the port should have bound");
    assert_eq!(stats.rtp.packets, 0);
    assert!(stats.since_packet_s.is_none(), "nothing has ever arrived");
    assert!(!stats.live());
}

#[test]
fn a_port_already_in_use_is_reported_rather_than_crashing() {
    // Hold the port for the whole test so the receiver cannot have it.
    let held = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("binding a port");
    let port = held.local_addr().expect("the bound address").port();

    let handle = video::spawn(
        egui::Context::default(),
        Source {
            bind: Ipv4Addr::LOCALHOST,
            port,
            codec: None,
        },
    );

    // The bind is attempted immediately, but give the thread a moment to run.
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if handle.stats().bind_error.is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("a port in use should be reported through bind_error");
}
