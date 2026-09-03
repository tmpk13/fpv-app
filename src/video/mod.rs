//! The video path: packets in, decoded frames and link statistics out.
//!
//! The shape is the one gps-gui-rs uses for its GPS and BLE sources: a
//! background thread produces, the UI drains each frame, and the platform
//! difference is confined to one submodule. Here that difference is the
//! decoder - GStreamer on desktop, `AMediaCodec` on Android - and everything
//! ahead of it ([`rtp`], [`codec`]) is shared and unit-tested.
//!
//! Where the packets come from is a second choice, made in [`source`]: the
//! radio, driven here through devourer and the wfb-ng link layer, or a UDP
//! port carrying RTP another machine has already unpacked. Both hand the loop
//! below the same thing, so nothing past this file knows which is in use.
//!
//! Frames reach the UI through a single-slot mailbox rather than a channel,
//! which is the one place this differs from the GPS source. A channel keeps
//! every value, and for video that is exactly wrong: if the UI falls behind,
//! the frames queue up and the picture drifts further and further into the
//! past while memory grows. The mailbox holds the newest frame and drops the
//! rest, so a slow frame costs a dropped picture instead of permanent latency.
//!
//! ```text
//! radio or udp --> Receiver thread --> Depayloader --> Decoder --> FrameSink
//!                       |                   |                         |
//!                   rate stats          loss stats              latest frame
//!                       \___________________|_________________________/
//!                                           v
//!                                     Stats + mailbox --> egui
//! ```

pub mod codec;
mod decode;
pub mod rtp;
pub mod source;
pub mod yuv;

use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub use codec::Codec;
pub use rtp::RtpStats;
pub use source::{Bandwidth, RadioSettings, Source, SourceKind, UdpSettings};

use codec::Detector;
use rtp::Depayloader;
use source::{PacketSource, UdpSource};

#[cfg(feature = "radio")]
use crate::radio::RadioStats;

/// Largest UDP datagram accepted. wfb-ng emits RTP inside a 1500-byte MTU, so
/// this has plenty of headroom while still bounding the read buffer.
const MAX_DATAGRAM: usize = 2048;

/// How long the receive socket blocks before looping.
///
/// This is what makes the thread responsive to a settings change and to
/// shutdown without a second wakeup mechanism: with no video arriving, the
/// loop still comes round four times a second to check for both.
const RECV_TIMEOUT: Duration = Duration::from_millis(250);

/// Window the bitrate and packet rate are averaged over.
const RATE_WINDOW: Duration = Duration::from_secs(1);

/// A decoded picture, ready to upload as a texture.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// Tightly packed RGBA, `width * height * 4` bytes with no row padding.
    /// The decoders undo their own stride before it gets here, because egui
    /// has nowhere to put one.
    pub rgba: Vec<u8>,
}

/// Everything the Link page reports.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    /// What the RTP layer saw.
    pub rtp: RtpStats,
    /// The codec in use, once enough packets agree on one.
    pub codec: Option<Codec>,
    /// Decoded picture size. Comes from the decoder rather than from the
    /// stream's SPS, so it is what is actually being displayed.
    pub width: u32,
    pub height: u32,
    /// Pictures out of the decoder, and pictures it refused.
    pub frames: u64,
    pub decode_errors: u64,
    /// Frames the UI never displayed because a newer one replaced them first.
    /// A steady count here means the display is the bottleneck, not the link.
    pub dropped_frames: u64,
    /// Averaged over [`RATE_WINDOW`].
    pub bitrate_bps: f64,
    pub packet_rate: f64,
    pub fps: f64,
    /// Seconds since the last packet and the last decoded frame. Both are
    /// needed: packets without frames is a decoder problem, neither is a link
    /// problem, and telling those apart from the picture alone is impossible.
    pub since_packet_s: Option<f64>,
    pub since_frame_s: Option<f64>,
    /// Why nothing is arriving, when the source itself knows: a port that
    /// would not bind, an adapter that is not there, a key file that is not
    /// readable. The one class of failure that leaves nothing else to report.
    pub fault: Option<String>,
    /// What the radio and the wfb-ng link layer saw. `None` when the source
    /// is a UDP port, because then this machine is not the ground station and
    /// has no way to know any of it.
    #[cfg(feature = "radio")]
    pub radio: Option<RadioStats>,
}

impl Stats {
    /// Whether video is arriving and decoding right now.
    pub fn live(&self) -> bool {
        self.since_frame_s.is_some_and(|s| s < 2.0)
    }
}

/// What the UI can ask the receive thread to do.
enum Command {
    /// Rebind to a new address, discarding the current decoder.
    Retune(Source),
    /// Throw away the decoder and codec vote and start again, for when a
    /// stream has changed under the app.
    Restart,
}

/// The UI's end of the video path.
pub struct VideoHandle {
    mailbox: Arc<Mutex<Option<Frame>>>,
    stats: Arc<Mutex<Stats>>,
    commands: Sender<Command>,
    source: Source,
}

impl VideoHandle {
    /// Take the newest decoded frame, if one has arrived since the last call.
    ///
    /// Returns the frame by value and empties the mailbox, so the texture
    /// upload does not hold the lock the decoder thread needs.
    pub fn take_frame(&self) -> Option<Frame> {
        self.mailbox.lock().ok()?.take()
    }

    pub fn stats(&self) -> Stats {
        self.stats.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn source(&self) -> &Source {
        &self.source
    }

    /// Point the receiver at a different source, or a different forced codec.
    pub fn retune(&mut self, source: Source) {
        if source == self.source {
            return;
        }
        self.source = source.clone();
        let _ = self.commands.send(Command::Retune(source));
    }

    /// Drop the decoder and the codec vote and start over.
    pub fn restart(&self) {
        let _ = self.commands.send(Command::Restart);
    }
}

/// Where a decoder puts the pictures it produces.
///
/// Cloned into the decoder thread. Writing a frame both fills the mailbox and
/// wakes the UI, which is why the egui context is in here: a decoded frame is
/// useless until something repaints, and nothing else would.
#[derive(Clone)]
pub struct FrameSink {
    mailbox: Arc<Mutex<Option<Frame>>>,
    stats: Arc<Mutex<Stats>>,
    ctx: egui::Context,
    /// Frame times inside the current rate window, for the fps readout.
    frame_window: Arc<Mutex<Vec<Instant>>>,
}

impl FrameSink {
    /// Publish a decoded picture, replacing any the UI has not collected.
    pub fn put(&self, frame: Frame) {
        let (width, height) = (frame.width, frame.height);

        let replaced = match self.mailbox.lock() {
            Ok(mut slot) => slot.replace(frame).is_some(),
            Err(_) => return,
        };

        let now = Instant::now();
        let fps = match self.frame_window.lock() {
            Ok(mut times) => {
                times.push(now);
                times.retain(|t| now.duration_since(*t) <= RATE_WINDOW);
                times.len() as f64 / RATE_WINDOW.as_secs_f64()
            }
            Err(_) => 0.0,
        };

        if let Ok(mut stats) = self.stats.lock() {
            stats.frames += 1;
            stats.width = width;
            stats.height = height;
            stats.fps = fps;
            stats.since_frame_s = Some(0.0);
            if replaced {
                // The previous frame was never displayed.
                stats.dropped_frames += 1;
            }
        }

        // Without this the picture only advances when something else asks for
        // a repaint, which on a still UI is nothing at all.
        self.ctx.request_repaint();
    }

    /// Record that the decoder rejected a picture.
    pub fn note_error(&self) {
        if let Ok(mut stats) = self.stats.lock() {
            stats.decode_errors += 1;
        }
    }
}

/// Start receiving on `source`, decoding into frames the UI can collect.
///
/// Never fails: a port that cannot be bound is reported through
/// [`Stats::bind_error`] and retried, because the usual cause is another copy
/// of the app (or a stray `gst-launch`) still holding the port, and that
/// clears on its own once the other one exits.
pub fn spawn(ctx: egui::Context, source: Source) -> VideoHandle {
    let mailbox = Arc::new(Mutex::new(None));
    let stats = Arc::new(Mutex::new(Stats::default()));
    let (tx, rx) = channel();

    let sink = FrameSink {
        mailbox: Arc::clone(&mailbox),
        stats: Arc::clone(&stats),
        ctx,
        frame_window: Arc::new(Mutex::new(Vec::new())),
    };

    let handle_source = source.clone();
    thread::Builder::new()
        .name("video-rx".into())
        .spawn(move || receive_loop(source, sink, rx))
        .expect("spawning the video receive thread");

    VideoHandle {
        mailbox,
        stats,
        commands: tx,
        source: handle_source,
    }
}

/// Receive, depayload, decode. Runs until the command channel closes, which
/// happens when the app drops its handle.
fn receive_loop(mut source: Source, sink: FrameSink, commands: Receiver<Command>) {
    let mut input = open(&source);
    let mut session = Session::new(source.codec);
    let mut scratch = vec![0u8; MAX_DATAGRAM];
    let mut rates = RateWindow::default();
    let mut last_packet: Option<Instant> = None;
    let mut last_frame_seen = 0u64;
    let mut last_frame_at: Option<Instant> = None;
    let mut session_started = Instant::now();
    let mut last_retry = Instant::now();
    // What the source last complained about, so a fault that persists is
    // reported once rather than every time it is retried.
    let mut reported_fault: Option<String> = None;

    loop {
        match commands.try_recv() {
            Ok(Command::Retune(next)) => {
                source = next;
                input = open(&source);
                reported_fault = None;
                session = Session::new(source.codec);
                rates = RateWindow::default();
                last_packet = None;
                last_frame_at = None;
                session_started = Instant::now();
                continue;
            }
            Ok(Command::Restart) => {
                session.restart();
                session_started = Instant::now();
                last_frame_at = None;
                continue;
            }
            // The app dropped its handle, so nothing will read the frames.
            Err(TryRecvError::Disconnected) => return,
            Err(TryRecvError::Empty) => {}
        }

        // The borrow of both `input` and `scratch` lasts as long as the
        // packet does, so everything that touches it happens in here.
        let arrived = match input.recv(&mut scratch, RECV_TIMEOUT) {
            Some(packet) => {
                rates.record(packet.len());
                session.feed(packet, &sink);
                true
            }
            None => false,
        };
        if arrived {
            last_packet = Some(Instant::now());
        }

        let fault = input.fault();
        if fault != reported_fault {
            match fault.as_deref() {
                Some(message) => log::error!("video: {message}"),
                None => log::info!("video: the source is up"),
            }
            reported_fault = fault.clone();
        }

        // The rate figures and the two "seconds since" clocks have to keep
        // moving while nothing is arriving - that silence is the reading.
        if let Ok(mut stats) = sink.stats.lock() {
            let (bitrate, packet_rate) = rates.rates();
            stats.bitrate_bps = bitrate;
            stats.packet_rate = packet_rate;
            stats.rtp = session.stats();
            stats.codec = session.codec();
            stats.since_packet_s = last_packet.map(|t| t.elapsed().as_secs_f64());
            // The sink stamps a frame's arrival as zero but cannot age it,
            // holding no clock of its own; this is where that is turned into
            // elapsed time.
            if stats.frames != last_frame_seen {
                last_frame_seen = stats.frames;
                last_frame_at = Some(Instant::now());
            }
            stats.since_frame_s = last_frame_at.map(|t| t.elapsed().as_secs_f64());
            stats.fault = fault.clone();
            #[cfg(feature = "radio")]
            {
                stats.radio = input.radio_stats();
            }
        }

        // A source that never opened, or that has stopped, is retried rather
        // than left broken: a port is usually held by something about to
        // exit, and an adapter is usually about to be plugged back in.
        if fault.is_some() && last_retry.elapsed() > RETRY_INTERVAL {
            last_retry = Instant::now();
            log::debug!("video: reopening the source");
            input = open(&source);
            session = Session::new(source.codec);
            session_started = Instant::now();
        }

        // Packets arriving with no pictures coming out means the decoder is
        // holding a stream it cannot decode - most often because the air unit
        // rebooted into the other codec, which nothing downstream would ever
        // notice on its own. Rebuilding the session re-runs detection.
        if stalled(last_packet, last_frame_at, session_started.elapsed()) {
            log::info!("video: packets but no frames, restarting the decoder");
            session.restart();
            session_started = Instant::now();
            last_frame_at = None;
        }
    }
}

/// How long to wait before reopening a source that failed.
///
/// Long enough that a missing adapter does not mean a USB reset every quarter
/// second, short enough that plugging one in is noticed while the user is
/// still looking at the screen.
const RETRY_INTERVAL: Duration = Duration::from_secs(3);

/// Build the source the settings describe.
fn open(source: &Source) -> Box<dyn PacketSource> {
    match source.kind {
        #[cfg(feature = "radio")]
        SourceKind::Radio => Box::new(source::RadioSource::new(&source.radio)),
        // Without the radio feature there is no adapter to open, so the
        // setting falls back to the port rather than reporting a fault the
        // user cannot fix from this build.
        #[cfg(not(feature = "radio"))]
        SourceKind::Radio => Box::new(UdpSource::new(source.udp, RECV_TIMEOUT)),
        SourceKind::Udp => Box::new(UdpSource::new(source.udp, RECV_TIMEOUT)),
    }
}

/// How long to accept packets without pictures before rebuilding the decoder.
///
/// Comfortably longer than a keyframe interval, which is what the wait for a
/// start point can legitimately cost when a stream begins.
const STALL_TIMEOUT: Duration = Duration::from_secs(5);

/// Whether packets are arriving but nothing is decoding.
///
/// All three clocks are needed:
///
/// - No packets at all is a link problem, and rebuilding the decoder would not
///   help.
/// - Packets without pictures is the decoder's problem, and rebuilding is the
///   only thing that does help.
/// - A session that has only just started has not had time to fail yet. This
///   is what `since_session` is for: without it, a decoder that has never
///   produced a frame reads as stalled from its first packet, and the loop
///   would rebuild it forever without ever giving it a keyframe to start on.
fn stalled(
    last_packet: Option<Instant>,
    last_frame: Option<Instant>,
    since_session: Duration,
) -> bool {
    let Some(packet) = last_packet else {
        return false;
    };
    // The link went quiet, so there is nothing to decode and nothing to blame
    // the decoder for.
    if packet.elapsed() > Duration::from_secs(1) {
        return false;
    }
    match last_frame {
        Some(frame) => frame.elapsed() > STALL_TIMEOUT,
        // Nothing has ever decoded, so the session's own age is the clock.
        None => since_session > STALL_TIMEOUT,
    }
}

/// One stream: the codec vote, the depayloader and the decoder built for it.
///
/// Held together because they are replaced together. The codec is not known
/// until packets have been seen, so the depayloader and the decoder cannot
/// exist before then, and if the codec ever changes all three are stale at
/// once.
struct Session {
    /// A codec forced by configuration, which skips detection entirely.
    forced: Option<Codec>,
    detector: Detector,
    depay: Option<Depayloader>,
    decoder: Option<Box<dyn decode::Decoder>>,
    /// Whether a unit the decoder can start from has been seen yet.
    started: bool,
    /// Stats from the depayloader that was replaced, so a restart does not
    /// reset the packet and loss counts the user is reading.
    carried: RtpStats,
}

impl Session {
    fn new(forced: Option<Codec>) -> Self {
        let mut session = Self {
            forced,
            detector: Detector::default(),
            depay: None,
            decoder: None,
            started: false,
            carried: RtpStats::default(),
        };
        if let Some(codec) = forced {
            session.depay = Some(Depayloader::new(codec));
        }
        session
    }

    /// Throw away the decoder and the codec vote, keeping the counts.
    ///
    /// This is what makes an air unit rebooted into the other codec recover on
    /// its own: the decoder is holding a stream it can no longer parse, and
    /// nothing short of building a new one for the new codec will fix it. The
    /// packet and loss totals are carried across because they describe the
    /// link, which did not restart - resetting them would erase the history
    /// the Link page exists to show.
    fn restart(&mut self) {
        self.carried = self.stats();
        self.detector.reset();
        self.depay = self.forced.map(Depayloader::new);
        self.decoder = None;
        self.started = false;
    }

    fn codec(&self) -> Option<Codec> {
        self.depay.as_ref().map(|d| d.codec())
    }

    /// The depayloader's counts, plus those of any depayloader it replaced.
    fn stats(&self) -> RtpStats {
        let Some(depay) = self.depay.as_ref() else {
            return self.carried;
        };
        let live = depay.stats();
        RtpStats {
            packets: self.carried.packets + live.packets,
            malformed: self.carried.malformed + live.malformed,
            lost: self.carried.lost + live.lost,
            reordered: self.carried.reordered + live.reordered,
            resets: self.carried.resets + live.resets,
            bytes: self.carried.bytes + live.bytes,
            access_units: self.carried.access_units + live.access_units,
            damaged: self.carried.damaged + live.damaged,
        }
    }

    /// Push one datagram through detection, depayloading and decode.
    fn feed(&mut self, datagram: &[u8], sink: &FrameSink) {
        // Detection needs the RTP payload, not the datagram, so the header has
        // to come off first. The depayloader does that itself once it exists.
        if self.depay.is_none() {
            let Some(payload) = datagram.get(rtp_payload_offset(datagram)..) else {
                return;
            };
            let Some(detected) = self.detector.push(payload) else {
                return;
            };
            log::info!("video: detected {detected}");
            self.depay = Some(Depayloader::new(detected));
        }

        let depay = self.depay.as_mut().expect("just built above");
        let Some(unit) = depay.push(datagram) else {
            return;
        };

        // A decoder handed mid-stream data before any parameter set has
        // nothing to configure itself from: GStreamer's parsers would sit
        // there dropping buffers, and AMediaCodec cannot even be configured.
        // Waiting for the first unit that carries one costs at most one
        // keyframe interval.
        if !self.started {
            if !unit.keyframe {
                return;
            }
            self.started = true;
        }

        if self.decoder.is_none() {
            let codec = depay.codec();
            match decode::new(codec, sink.clone()) {
                Ok(decoder) => self.decoder = Some(decoder),
                Err(err) => {
                    log::error!("video: cannot start the {codec} decoder: {err}");
                    // Fall back to waiting for the next start point rather
                    // than retrying on every unit, which would spin.
                    self.started = false;
                    sink.note_error();
                    return;
                }
            }
        }

        if let Some(decoder) = self.decoder.as_mut() {
            if let Err(err) = decoder.submit(&unit) {
                log::warn!("video: decode failed: {err}");
                sink.note_error();
            }
        }
    }
}

/// Offset of the payload inside an RTP datagram, for the codec detector.
///
/// A deliberately forgiving version of the parser in [`rtp`]: it only has to
/// land somewhere the first NAL header can be read, and a datagram it
/// misjudges costs one vote out of five rather than a wrong answer.
fn rtp_payload_offset(datagram: &[u8]) -> usize {
    let Some(&first) = datagram.first() else {
        return 0;
    };
    12 + 4 * usize::from(first & 0x0f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_offset_accounts_for_csrc_entries() {
        assert_eq!(rtp_payload_offset(&[0x80]), 12);
        assert_eq!(rtp_payload_offset(&[0x82]), 20);
        assert_eq!(rtp_payload_offset(&[]), 0);
    }

    #[test]
    fn a_session_carries_its_counts_across_a_codec_change() {
        let mut session = Session::new(Some(Codec::H264));
        // Two packets through the depayloader that exists now.
        let mut packet = vec![0x80, 0x00, 0x00, 0x01];
        packet.extend_from_slice(&[0; 8]);
        packet.extend_from_slice(&[0x41, 0x01]);
        session.depay.as_mut().unwrap().push(&packet);
        let before = session.stats().packets;
        assert_eq!(before, 1);

        // Simulate the swap the feed path makes when the codec changes.
        session.carried = session.stats();
        session.depay = Some(Depayloader::new(Codec::H265));
        assert_eq!(
            session.stats().packets,
            before,
            "a codec change must not reset the counters the user is reading"
        );
    }

    #[test]
    fn loss_percentage_is_over_expected_packets() {
        let stats = RtpStats {
            packets: 90,
            lost: 10,
            ..RtpStats::default()
        };
        assert!((stats.loss_pct() - 10.0).abs() < 0.01);
    }

    #[test]
    fn a_quiet_link_is_not_a_stalled_decoder() {
        let long_ago = Instant::now() - Duration::from_secs(30);
        assert!(
            !stalled(Some(long_ago), None, Duration::from_secs(60)),
            "no packets means nothing to decode; rebuilding would not help"
        );
        assert!(!stalled(None, None, Duration::from_secs(60)));
    }

    #[test]
    fn packets_with_no_frames_eventually_counts_as_stalled() {
        let now = Instant::now();
        // The session has only just started, so the decoder has not had time
        // to find a keyframe yet.
        assert!(!stalled(Some(now), None, Duration::from_millis(500)));
        // Long enough that a keyframe should have arrived.
        assert!(stalled(
            Some(now),
            None,
            STALL_TIMEOUT + Duration::from_secs(1)
        ));
    }

    #[test]
    fn a_decoder_that_stopped_producing_counts_as_stalled() {
        let now = Instant::now();
        let stale_frame = now - STALL_TIMEOUT - Duration::from_secs(1);
        assert!(stalled(
            Some(now),
            Some(stale_frame),
            Duration::from_secs(60)
        ));
        // A frame just now is not a stall, however old the session is.
        assert!(!stalled(Some(now), Some(now), Duration::from_secs(600)));
    }

    #[test]
    fn stats_are_not_live_without_a_recent_frame() {
        let mut stats = Stats::default();
        assert!(!stats.live(), "nothing has ever arrived");
        stats.since_frame_s = Some(5.0);
        assert!(!stats.live());
        stats.since_frame_s = Some(0.1);
        assert!(stats.live());
    }
}

/// A rolling window of packet arrivals, for the bitrate and packet-rate
/// readouts.
///
/// Timestamps are kept rather than a running total so the rate falls to zero
/// on its own when the link drops. A counter divided by elapsed time would
/// instead decay slowly and read as a link that is merely slow, which is the
/// opposite of what has happened.
#[derive(Default)]
struct RateWindow {
    /// `(arrival, payload bytes)` for the packets inside the window.
    packets: Vec<(Instant, usize)>,
}

impl RateWindow {
    fn record(&mut self, bytes: usize) {
        self.packets.push((Instant::now(), bytes));
    }

    /// `(bits per second, packets per second)` over [`RATE_WINDOW`].
    fn rates(&mut self) -> (f64, f64) {
        let now = Instant::now();
        self.packets
            .retain(|(t, _)| now.duration_since(*t) <= RATE_WINDOW);
        let bytes: usize = self.packets.iter().map(|(_, n)| n).sum();
        let window = RATE_WINDOW.as_secs_f64();
        (
            8.0 * bytes as f64 / window,
            self.packets.len() as f64 / window,
        )
    }
}
