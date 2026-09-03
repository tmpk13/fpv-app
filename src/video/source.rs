// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Where the video comes from: the radio, or a UDP port.
//!
//! Both produce the same thing - RTP packets - so everything downstream is
//! identical and the choice is one line in the config.
//!
//! - **Radio.** A Realtek adapter on USB, driven by devourer, with the wfb-ng
//!   link layer on top. This machine is the whole ground station.
//! - **UDP.** RTP already decrypted and reassembled by someone else's
//!   `wfb_rx`, forwarded over the network. Needs a laptop but no adapter, and
//!   it is also how a phone watches a link a ground station is already
//!   receiving.
//!
//! The trait between them is deliberately as narrow as a socket: give me the
//! next packet, and tell me if you have broken. Anything a radio knows and a
//! socket does not - signal, erasure coding, session keys - travels as
//! statistics rather than through the packet path, so the receive loop does
//! not have to know which one it is holding.

use std::net::UdpSocket;
use std::path::PathBuf;
use std::time::Duration;

use super::Codec;

#[cfg(feature = "radio")]
use crate::radio::{Radio, RadioConfig, RadioStats};

/// Which source the app is using.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SourceKind {
    /// Drive the adapter here. The default, and the point of the app.
    #[default]
    Radio,
    /// Take RTP that another machine's `wfb_rx` already unpacked.
    Udp,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Radio => "radio",
            Self::Udp => "udp",
        }
    }
}

/// Settings for the forwarded-RTP source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UdpSettings {
    /// `0.0.0.0` receives from anywhere on the network, which is what a phone
    /// needs; `127.0.0.1` is enough when `wfb_rx` runs on this machine.
    pub bind: std::net::Ipv4Addr,
    pub port: u16,
}

impl Default for UdpSettings {
    fn default() -> Self {
        Self {
            bind: std::net::Ipv4Addr::UNSPECIFIED,
            // The port drone-cam's `wfb_rx -u` unpacks video to.
            port: 5600,
        }
    }
}

/// Settings for the radio source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RadioSettings {
    pub channel: u8,
    pub bandwidth: Bandwidth,
    /// Must match the air unit's `wfb_tx -i`.
    pub link_id: u32,
    pub radio_port: u8,
    /// Where `gs.key` is. Resolved against the config file's directory when
    /// relative, so the pair travel together.
    pub key_path: PathBuf,
}

impl Default for RadioSettings {
    fn default() -> Self {
        Self {
            // drone-cam's defaults, so two checkouts of these repositories
            // talk to each other with nothing configured at either end.
            channel: 161,
            bandwidth: Bandwidth::Mhz20,
            link_id: 7669206,
            radio_port: 0,
            key_path: PathBuf::from("gs.key"),
        }
    }
}

/// Channel width. Mirrors [`crate::radio::Bandwidth`], and exists separately
/// so the config and the UI still compile without the radio feature.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Bandwidth {
    #[default]
    Mhz20,
    Mhz40,
}

impl Bandwidth {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mhz20 => "20 MHz",
            Self::Mhz40 => "40 MHz",
        }
    }

    pub fn from_mhz(mhz: u32) -> Self {
        if mhz >= 40 {
            Self::Mhz40
        } else {
            Self::Mhz20
        }
    }

    pub fn mhz(self) -> u32 {
        match self {
            Self::Mhz20 => 20,
            Self::Mhz40 => 40,
        }
    }
}

/// Everything the receive thread is configured with.
///
/// Both sources' settings are kept, not just the selected one's, so switching
/// between them on the Settings page and back does not lose what was typed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Source {
    pub kind: SourceKind,
    pub udp: UdpSettings,
    pub radio: RadioSettings,
    /// Force a codec instead of detecting one. Detection costs a few packets
    /// at startup and is right every time in practice, so this is for pinning
    /// the answer when a stream is known to be misclassified.
    pub codec: Option<Codec>,
}

/// A packet source, seen from the receive loop.
pub trait PacketSource: Send {
    /// The next RTP packet, or `None` if none arrived within `timeout`.
    ///
    /// `scratch` is somewhere to put a packet that has nowhere else to live;
    /// a source that already owns its buffer may return its own instead. It
    /// exists so the socket path copies once rather than allocating per
    /// datagram.
    fn recv<'a>(&'a mut self, scratch: &'a mut [u8], timeout: Duration) -> Option<&'a [u8]>;

    /// What is stopping packets arriving, if anything is.
    fn fault(&self) -> Option<String>;

    /// Counters only a radio has.
    #[cfg(feature = "radio")]
    fn radio_stats(&self) -> Option<RadioStats> {
        None
    }
}

/// RTP forwarded from another machine's `wfb_rx`.
pub struct UdpSource {
    socket: Option<UdpSocket>,
    settings: UdpSettings,
    fault: Option<String>,
}

impl UdpSource {
    /// Bind, recording a failure rather than returning one.
    ///
    /// A port that cannot be bound is not fatal: the usual cause is another
    /// copy of the app, or a stray `gst-launch`, still holding it, and that
    /// clears on its own once the other one exits. The loop retries.
    pub fn new(settings: UdpSettings, timeout: Duration) -> Self {
        let result = UdpSocket::bind((settings.bind, settings.port))
            .and_then(|socket| socket.set_read_timeout(Some(timeout)).map(|()| socket));

        let (socket, fault) = match result {
            Ok(socket) => {
                log::info!("video: listening on {}:{}", settings.bind, settings.port);
                (Some(socket), None)
            }
            Err(err) => {
                log::debug!(
                    "video: cannot bind {}:{}: {err}",
                    settings.bind,
                    settings.port
                );
                let reason = match err.kind() {
                    std::io::ErrorKind::AddrInUse => "port already in use",
                    std::io::ErrorKind::PermissionDenied => "permission denied",
                    _ => "cannot bind the port",
                };
                (
                    None,
                    Some(format!(
                        "cannot listen on {}:{}: {reason}",
                        settings.bind, settings.port
                    )),
                )
            }
        };
        Self {
            socket,
            settings,
            fault,
        }
    }

    /// Whether the socket is bound. A failed bind is retried by the loop.
    pub fn is_bound(&self) -> bool {
        self.socket.is_some()
    }

    pub fn settings(&self) -> UdpSettings {
        self.settings
    }
}

impl PacketSource for UdpSource {
    fn recv<'a>(&'a mut self, scratch: &'a mut [u8], timeout: Duration) -> Option<&'a [u8]> {
        let socket = self.socket.as_ref()?;
        match socket.recv(scratch) {
            Ok(len) => Some(&scratch[..len]),
            Err(err) if would_block(&err) => None,
            Err(err) => {
                log::warn!("video socket error: {err}");
                std::thread::sleep(timeout);
                None
            }
        }
    }

    fn fault(&self) -> Option<String> {
        self.fault.clone()
    }
}

/// Whether a socket error is just the read timeout expiring.
///
/// Both kinds are checked because the platforms disagree: a timed-out `recv`
/// is `WouldBlock` on Unix and `TimedOut` on Windows, and treating either as
/// a real error would log a warning four times a second on an idle link.
fn would_block(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

/// The adapter, through devourer and the wfb-ng link layer.
#[cfg(feature = "radio")]
pub struct RadioSource {
    radio: Option<Radio>,
    /// Set when the adapter could not be opened at all, which is a different
    /// thing from one that opened and then stopped.
    open_error: Option<String>,
    packet: Vec<u8>,
    /// Kept alive for as long as the adapter is: on Android the descriptor
    /// belongs to a Java object, and letting it go closes the device.
    #[cfg(target_os = "android")]
    _usb: Option<crate::radio::android::UsbHandle>,
}

#[cfg(feature = "radio")]
impl RadioSource {
    pub fn new(settings: &RadioSettings) -> Self {
        match Self::try_open(settings) {
            Ok(source) => source,
            Err(error) => {
                // Reported by the receive loop rather than here: this is
                // retried every few seconds, and logging from the constructor
                // puts the same line in the log until the adapter appears.
                log::debug!("radio: {error}");
                Self {
                    radio: None,
                    open_error: Some(error),
                    packet: Vec::new(),
                    #[cfg(target_os = "android")]
                    _usb: None,
                }
            }
        }
    }

    fn try_open(settings: &RadioSettings) -> Result<Self, String> {
        let key = std::fs::read(&settings.key_path).map_err(|err| {
            format!(
                "cannot read the key file {}: {err}",
                settings.key_path.display()
            )
        })?;

        let config = RadioConfig {
            channel: settings.channel,
            bandwidth: match settings.bandwidth {
                Bandwidth::Mhz20 => crate::radio::Bandwidth::Mhz20,
                Bandwidth::Mhz40 => crate::radio::Bandwidth::Mhz40,
            },
            link_id: settings.link_id,
            radio_port: settings.radio_port,
            key,
            vid: 0,
            pid: 0,
        };

        // On Android the app never enumerates USB: it is handed a descriptor
        // for the one device the user granted, and libusb adopts it.
        #[cfg(target_os = "android")]
        {
            let usb = crate::radio::android::open_adapter(None)?;
            let radio = Radio::open(config, Some(usb.fd()))?;
            Ok(Self {
                radio: Some(radio),
                open_error: None,
                packet: Vec::new(),
                _usb: Some(usb),
            })
        }

        #[cfg(not(target_os = "android"))]
        {
            let radio = Radio::open(config, None)?;
            Ok(Self {
                radio: Some(radio),
                open_error: None,
                packet: Vec::new(),
            })
        }
    }

    /// Whether the adapter is open. A failed open is retried by the loop.
    pub fn is_open(&self) -> bool {
        self.radio.is_some()
    }
}

#[cfg(feature = "radio")]
impl PacketSource for RadioSource {
    fn recv<'a>(&'a mut self, _scratch: &'a mut [u8], timeout: Duration) -> Option<&'a [u8]> {
        let radio = self.radio.as_mut()?;
        // The link layer already produced a whole packet, so there is nothing
        // to copy into the scratch buffer: hand back the one it made.
        self.packet = radio.recv(timeout)?;
        Some(&self.packet)
    }

    fn fault(&self) -> Option<String> {
        if let Some(error) = self.open_error.as_ref() {
            return Some(error.clone());
        }
        self.radio.as_ref().and_then(|radio| radio.fault())
    }

    fn radio_stats(&self) -> Option<RadioStats> {
        self.radio.as_ref().map(|radio| radio.stats())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_source_drives_the_radio() {
        // The whole point of the rebuild: a fresh install is a ground
        // station, not a viewer waiting for someone else to be one.
        assert_eq!(Source::default().kind, SourceKind::Radio);
    }

    #[test]
    fn both_sources_settings_survive_switching() {
        let mut source = Source::default();
        source.udp.port = 5601;
        source.kind = SourceKind::Udp;
        // Switching to the radio and back must not have lost the port.
        source.kind = SourceKind::Radio;
        source.kind = SourceKind::Udp;
        assert_eq!(source.udp.port, 5601);
    }

    #[test]
    fn a_port_that_is_already_taken_is_reported_not_thrown() {
        let held = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let port = held.local_addr().unwrap().port();
        let settings = UdpSettings {
            bind: std::net::Ipv4Addr::LOCALHOST,
            port,
        };
        // A second bind of the same port fails; the source must come back
        // with an explanation rather than not coming back.
        let source = UdpSource::new(settings, Duration::from_millis(10));
        assert!(!source.is_bound());
        assert!(source.fault().is_some_and(|f| f.contains("in use")));
    }

    #[test]
    fn a_bound_socket_has_no_fault_and_times_out_quietly() {
        let settings = UdpSettings {
            bind: std::net::Ipv4Addr::LOCALHOST,
            port: 0,
        };
        let mut source = UdpSource::new(settings, Duration::from_millis(10));
        assert!(source.is_bound());
        assert!(source.fault().is_none());
        let mut scratch = [0u8; 64];
        assert!(source
            .recv(&mut scratch, Duration::from_millis(10))
            .is_none());
    }

    #[test]
    fn bandwidth_round_trips_through_its_config_form() {
        assert_eq!(Bandwidth::from_mhz(20), Bandwidth::Mhz20);
        assert_eq!(Bandwidth::from_mhz(40), Bandwidth::Mhz40);
        // Anything else is read as the narrower one rather than refused: a
        // typo in the config should cost bandwidth, not the video.
        assert_eq!(Bandwidth::from_mhz(80), Bandwidth::Mhz40);
        assert_eq!(Bandwidth::from_mhz(0), Bandwidth::Mhz20);
        assert_eq!(Bandwidth::Mhz40.mhz(), 40);
    }
}
