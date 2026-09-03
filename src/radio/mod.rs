// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! The radio: a USB adapter in monitor mode, feeding the wfb-ng link layer.
//!
//! This is the half of a ground station that used to be a laptop running
//! `wfb_rx`. devourer drives the Realtek chip from userspace over libusb -
//! no kernel module, no root - and every frame it hears goes through
//! [`crate::wfb`], which decrypts and reassembles it into the same RTP
//! packets the UDP path used to receive.
//!
//! ```text
//!   RTL8812AU on USB
//!         |  libusb
//!    devourer (C++)         one thread, inside the library
//!         |  dv_rx_callback
//!    Link::push_frame       wfb-ng: filter, decrypt, FEC, reorder
//!         |  bounded queue
//!    Radio::recv            the video thread, as if it were a socket
//! ```
//!
//! The queue between the two threads is bounded and drops its newest entry
//! when full, for the same reason the frame mailbox holds one picture: a
//! receive path that buffers without limit turns a slow decoder into
//! unbounded latency, and on FPV video latency is the thing being bought.
//!
//! Every unsafe block in this crate is in here or in [`ffi`], and they all
//! rest on one guarantee the shim makes: `dv_stop` joins devourer's receive
//! thread, so once it returns no callback can be running.

pub mod ffi;

#[cfg(target_os = "android")]
pub mod android;

use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Mutex;
use std::time::Duration;

use crate::wfb::{self, LinkStats};

/// How many reassembled packets may wait for the video thread.
///
/// A second of video at the rates this link runs. Enough to ride out a
/// decoder hiccup, short enough that it cannot become latency.
const QUEUE_DEPTH: usize = 256;

/// Channel width. wfb-ng runs HT20 or HT40; nothing wider is useful here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Bandwidth {
    #[default]
    Mhz20,
    Mhz40,
}

impl Bandwidth {
    fn to_ffi(self) -> u8 {
        match self {
            Self::Mhz20 => ffi::DV_WIDTH_20,
            Self::Mhz40 => ffi::DV_WIDTH_40,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mhz20 => "20 MHz",
            Self::Mhz40 => "40 MHz",
        }
    }
}

/// Which adapter to open, where to listen, and whose traffic to accept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RadioConfig {
    pub channel: u8,
    pub bandwidth: Bandwidth,
    /// Must match the air unit's `wfb_tx -i`. A wrong value discards every
    /// frame before anything is decrypted, and looks exactly like an air unit
    /// that is switched off.
    pub link_id: u32,
    pub radio_port: u8,
    /// The contents of `gs.key`, not its path: on Android the file has to be
    /// copied into the app's own storage anyway, and holding the bytes keeps
    /// the file system out of the receive path.
    pub key: Vec<u8>,
    /// Pin one adapter by USB id. Zeroes mean the first supported one, which
    /// is what a ground station with one dongle wants.
    pub vid: u16,
    pub pid: u16,
}

impl Default for RadioConfig {
    fn default() -> Self {
        Self {
            // drone-cam's defaults, so a ground station built from these two
            // repositories works with neither end configured.
            channel: 161,
            bandwidth: Bandwidth::Mhz20,
            link_id: 7669206,
            radio_port: 0,
            key: Vec::new(),
            vid: 0,
            pid: 0,
        }
    }
}

/// Signal quality of the frames that were actually ours.
///
/// Averaging over every frame on the channel would measure the neighbours'
/// access point, so only frames that passed the link id filter count.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Signal {
    /// Strongest antenna, in dBm. `None` until a frame of ours arrives.
    pub rssi_dbm: Option<i32>,
    /// Signal to noise on that antenna, in dB.
    pub snr_db: Option<i32>,
    /// The estimate of the noise the receiver is working against: what is
    /// left when the signal is taken out of the total.
    pub noise_dbm: Option<i32>,
    /// Per-antenna dBm, for telling a dead antenna from a weak link.
    pub antennas: [Option<i32>; 4],
}

/// What the radio path reports to the Link page.
#[derive(Clone, Debug, Default)]
pub struct RadioStats {
    pub link: LinkStats,
    pub signal: Signal,
    /// The chip that was actually found, e.g. "RTL8812A".
    pub chip: String,
    /// Packets reassembled but never collected, because the video thread was
    /// too far behind.
    pub queue_drops: u64,
}

/// Everything the receive callback touches.
///
/// Boxed and kept alive by [`Radio`] for exactly as long as devourer's
/// thread can call into it.
struct RxState {
    /// devourer calls from one thread, so this is never contended by the
    /// producer; the lock is for the UI reading counters underneath it.
    link: Mutex<wfb::Link>,
    signal: Mutex<Signal>,
    packets: SyncSender<Vec<u8>>,
    queue_drops: AtomicU64,
}

/// An open adapter, receiving.
pub struct Radio {
    /// Owned by this struct and closed in [`Drop`], which is also what joins
    /// devourer's thread.
    device: *mut ffi::DvDevice,
    /// Must outlive `device`: the receive thread holds a pointer to it.
    state: Box<RxState>,
    packets: Receiver<Vec<u8>>,
    chip: String,
    config: RadioConfig,
}

// SAFETY: every field is either owned outright or synchronized. The raw
// pointer is only ever passed back to the shim, whose entry points are
// documented as callable from any single thread at a time, and `Radio` is
// moved to the video thread once and never shared.
unsafe impl Send for Radio {}

impl Radio {
    /// Open the adapter and start receiving.
    ///
    /// On Android the descriptor comes from `UsbManager` and this end never
    /// enumerates anything; on desktop the adapter is found by USB id.
    pub fn open(config: RadioConfig, usb_fd: Option<i32>) -> Result<Self, String> {
        let keys = wfb::crypto::KeyPair::from_bytes(&config.key)
            .ok_or_else(|| "the key file is not 64 bytes of gs.key".to_string())?;

        let mut err = ffi::ErrorBuffer::new();
        // SAFETY: both entry points either return a device this call owns or
        // NULL with a message in the buffer, whose length is passed with it.
        let device = unsafe {
            match usb_fd {
                Some(fd) => ffi::dv_open_fd(fd, err.as_mut_ptr(), err.capacity()),
                None => ffi::dv_open_usb(config.vid, config.pid, err.as_mut_ptr(), err.capacity()),
            }
        };
        if device.is_null() {
            return Err(err.take());
        }

        // SAFETY: `device` is non-null and the string is owned by it, so it
        // outlives the copy taken here.
        let chip = unsafe { ffi::borrowed_string(ffi::dv_chip_name(device)) }
            .unwrap_or_else(|| "unknown".into());

        let (tx, rx) = sync_channel(QUEUE_DEPTH);
        let channel_id = wfb::channel_id(config.link_id, config.radio_port);
        let state = Box::new(RxState {
            link: Mutex::new(wfb::Link::new(channel_id, keys)),
            signal: Mutex::new(Signal::default()),
            packets: tx,
            queue_drops: AtomicU64::new(0),
        });

        let mut err = ffi::ErrorBuffer::new();
        // SAFETY: `state` is boxed and stored in the returned `Radio`, which
        // calls `dv_close` before dropping it - and `dv_close` joins the
        // thread that holds this pointer, so it cannot outlive the box.
        let rc = unsafe {
            ffi::dv_start(
                device,
                config.channel,
                config.bandwidth.to_ffi(),
                0,
                on_packet,
                &*state as *const RxState as *mut c_void,
                err.as_mut_ptr(),
                err.capacity(),
            )
        };
        if rc != 0 {
            // SAFETY: nothing was started, so no thread can be running.
            unsafe { ffi::dv_close(device) };
            return Err(err.take());
        }

        log::info!(
            "radio: {chip} on channel {} {}, link {} port {}",
            config.channel,
            config.bandwidth.as_str(),
            config.link_id,
            config.radio_port
        );

        Ok(Self {
            device,
            state,
            packets: rx,
            chip,
            config,
        })
    }

    /// The next reassembled packet, or `None` if none arrived in `timeout`.
    pub fn recv(&mut self, timeout: Duration) -> Option<Vec<u8>> {
        self.packets.recv_timeout(timeout).ok()
    }

    pub fn stats(&self) -> RadioStats {
        RadioStats {
            link: self
                .state
                .link
                .lock()
                .map(|link| link.stats())
                .unwrap_or_default(),
            signal: self
                .state
                .signal
                .lock()
                .map(|signal| *signal)
                .unwrap_or_default(),
            chip: self.chip.clone(),
            queue_drops: self.state.queue_drops.load(Ordering::Relaxed),
        }
    }

    pub fn config(&self) -> &RadioConfig {
        &self.config
    }

    /// Why receiving stopped, if it has.
    ///
    /// The one failure with no other symptom: an adapter pulled out of the
    /// port makes frames stop arriving and nothing else.
    pub fn fault(&self) -> Option<String> {
        // SAFETY: `self.device` is non-null for the lifetime of `self`, and
        // both calls are documented as safe from any thread.
        unsafe {
            if ffi::dv_running(self.device) {
                return None;
            }
            Some(
                ffi::borrowed_string(ffi::dv_rx_error(self.device))
                    .unwrap_or_else(|| "the adapter stopped receiving".into()),
            )
        }
    }

    /// Retune without reopening the adapter.
    pub fn set_channel(&mut self, channel: u8, bandwidth: Bandwidth) -> Result<(), String> {
        let mut err = ffi::ErrorBuffer::new();
        // SAFETY: `self.device` is non-null, and the control plane is single
        // threaded because `&mut self` is the only way in.
        let rc = unsafe {
            ffi::dv_set_channel(
                self.device,
                channel,
                bandwidth.to_ffi(),
                0,
                err.as_mut_ptr(),
                err.capacity(),
            )
        };
        if rc != 0 {
            return Err(err.take());
        }
        self.config.channel = channel;
        self.config.bandwidth = bandwidth;
        Ok(())
    }
}

impl std::fmt::Debug for Radio {
    /// Names the adapter and nothing else: the interesting state is behind a
    /// lock the receive thread is usually holding.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Radio({}, channel {})", self.chip, self.config.channel)
    }
}

impl Drop for Radio {
    fn drop(&mut self) {
        // SAFETY: this joins devourer's receive thread before returning, so
        // `self.state` - which that thread holds a pointer to - is not
        // dropped while anything can still reach it. Field order does not
        // save us here; the join does.
        unsafe { ffi::dv_close(self.device) };
    }
}

/// devourer's receive thread, arriving in Rust.
///
/// Everything this does happens on that thread, including the decryption and
/// the erasure coding. That is deliberate: the alternative is copying every
/// frame on the channel - other people's included - across a queue first, and
/// the link id filter throws most of them away in two comparisons.
extern "C" fn on_packet(user: *mut c_void, packet: *const ffi::DvPacket) {
    // A panic unwinding into C++ is undefined behaviour, so it stops here.
    // Nothing below should panic - the link layer is fuzzed against arbitrary
    // frames - but "should" is not a calling convention.
    let result = std::panic::catch_unwind(|| {
        if user.is_null() || packet.is_null() {
            return;
        }
        // SAFETY: `user` is the `RxState` box owned by the `Radio` that
        // started this thread, and `dv_close` joins the thread before that
        // box is dropped. `packet` is valid for this call by the shim's
        // contract.
        let state = unsafe { &*(user as *const RxState) };
        let packet = unsafe { &*packet };

        let Ok(mut link) = state.link.lock() else {
            return;
        };

        if packet.crc_error {
            link.note_crc_error();
            return;
        }
        if packet.data.is_null() || packet.len == 0 {
            return;
        }

        // SAFETY: the shim guarantees `data` points at `len` readable bytes
        // for the duration of this call, and the slice does not escape it.
        let frame = unsafe { std::slice::from_raw_parts(packet.data, packet.len) };

        let before = link.stats().frames;
        link.push_frame(frame, &mut |payload| {
            match state.packets.try_send(payload.to_vec()) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    state.queue_drops.fetch_add(1, Ordering::Relaxed);
                }
                // The video thread is gone; the radio is about to be closed.
                Err(TrySendError::Disconnected(_)) => {}
            }
        });

        // Signal is only meaningful for frames that were ours. Every other
        // frame on the channel belongs to somebody else's network, and
        // averaging those in would measure the neighbours.
        if link.stats().frames > before {
            if let Ok(mut signal) = state.signal.lock() {
                *signal = read_signal(packet);
            }
        }
    });

    if result.is_err() {
        log::error!("radio: the receive callback panicked; the frame was dropped");
    }
}

/// Turn Realtek's raw path gains into decibels.
///
/// The chip reports a gain index rather than a power: `raw - 110` is dBm and
/// the SNR is in half-decibels. A raw zero is not a very weak signal, it is
/// no reading at all - an antenna the adapter does not have, or a frame whose
/// PHY status the chip did not fill in.
fn read_signal(packet: &ffi::DvPacket) -> Signal {
    let mut antennas = [None; 4];
    let mut best = None;
    let mut best_snr = None;

    for (i, slot) in antennas.iter_mut().enumerate() {
        if packet.rssi[i] == 0 {
            continue;
        }
        let dbm = i32::from(packet.rssi[i]) - 110;
        *slot = Some(dbm);
        if best.is_none_or(|current| dbm > current) {
            best = Some(dbm);
            best_snr = Some(i32::from(packet.snr[i]) / 2);
        }
    }

    Signal {
        rssi_dbm: best,
        snr_db: best_snr,
        // What is left of the received power once the signal is taken out.
        noise_dbm: best.zip(best_snr).map(|(rssi, snr)| rssi - snr),
        antennas,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(rssi: [u8; 4], snr: [i8; 4]) -> ffi::DvPacket {
        ffi::DvPacket {
            data: std::ptr::null(),
            len: 0,
            rssi,
            snr,
            tsf: 0,
            rate: 0,
            bandwidth: 20,
            crc_error: false,
        }
    }

    #[test]
    fn raw_path_gain_becomes_dbm() {
        let signal = read_signal(&packet([70, 0, 0, 0], [40, 0, 0, 0]));
        assert_eq!(signal.rssi_dbm, Some(-40));
        assert_eq!(signal.snr_db, Some(20));
        assert_eq!(signal.noise_dbm, Some(-60));
    }

    #[test]
    fn the_strongest_antenna_is_the_reading() {
        let signal = read_signal(&packet([50, 80, 0, 0], [20, 60, 0, 0]));
        assert_eq!(signal.rssi_dbm, Some(-30), "antenna B is the strong one");
        assert_eq!(signal.snr_db, Some(30), "and its own SNR goes with it");
        assert_eq!(signal.antennas[0], Some(-60));
        assert_eq!(signal.antennas[1], Some(-30));
    }

    #[test]
    fn an_absent_antenna_is_not_a_weak_one() {
        // A 2T2R adapter leaves paths C and D at zero. Reading those as
        // -110 dBm would drag any average into a link that looks broken.
        let signal = read_signal(&packet([60, 58, 0, 0], [30, 28, 0, 0]));
        assert_eq!(signal.antennas[2], None);
        assert_eq!(signal.antennas[3], None);
        assert_eq!(signal.rssi_dbm, Some(-50));
    }

    #[test]
    fn a_frame_with_no_reading_at_all_reports_nothing() {
        let signal = read_signal(&packet([0; 4], [0; 4]));
        assert_eq!(signal.rssi_dbm, None);
        assert_eq!(signal.noise_dbm, None);
    }

    #[test]
    fn the_default_config_matches_the_ground_station_it_pairs_with() {
        let config = RadioConfig::default();
        assert_eq!(config.channel, 161);
        assert_eq!(config.link_id, 7669206);
        assert_eq!(
            wfb::channel_id(config.link_id, config.radio_port),
            7669206 << 8
        );
    }

    #[test]
    fn a_key_that_is_not_a_key_is_refused_before_the_adapter_is_touched() {
        let config = RadioConfig {
            key: vec![0; 10],
            ..RadioConfig::default()
        };
        // No USB is opened: the error comes from the key check, which is
        // first precisely so a typo does not cost an adapter reset.
        let error = Radio::open(config, None).unwrap_err();
        assert!(error.contains("64 bytes"), "{error}");
    }
}
