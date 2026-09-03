//! The app's prose: page hints, hover texts, and the sentences that explain a
//! link that is not working.
//!
//! Kept in one file so the wording can be read and revised as writing, rather
//! than found one string at a time among the layout code.

/// The corner toggle.
pub const MENU_OPEN: &str = "Menu";
pub const MENU_CLOSE: &str = "Close the menu";

/// Video page.
pub const VIDEO_HINT: &str =
    "Tap the picture to switch between fitting the whole frame and filling the screen.";

/// Link page.
pub const LINK_HINT: &str =
    "What the RTP layer sees, counted since the app started. Loss here is normal on a 5.8 GHz link; a steady rise is the margin going.";
pub const LINK_RESTART: &str =
    "Throw away the decoder and detect the codec again. Use this if the air unit was rebooted into a different codec.";

/// Settings page.
pub const SETTINGS_SOURCE_HINT: &str =
    "Radio drives the adapter on this device and receives the link itself. Forwarded takes RTP another machine's wfb_rx has already decrypted.";
pub const SETTINGS_RADIO_HINT: &str =
    "All four must match the air unit. In the drone-cam checkout, `sudo ./vrx.sh scan` reads the channel and link id off the air.";
pub const SETTINGS_CHANNEL_HINT: &str =
    "The 802.11 channel number, not a frequency. 149 to 165 is the 5.8 GHz band FPV normally uses.";
pub const SETTINGS_LINK_HINT: &str =
    "The receiver filters on this and the radio port together, so a wrong value discards every frame and looks exactly like an air unit that is switched off.";
pub const SETTINGS_KEY_HINT: &str =
    "The ground station half of the wfb_keygen pair, gs.key. A relative path is resolved beside the config file.";
pub const SETTINGS_UDP_HINT: &str =
    "Where the forwarded RTP arrives. These must match what wfb_rx was told to unpack to.";
pub const SETTINGS_BIND_HINT: &str =
    "0.0.0.0 receives from anywhere on the network. 127.0.0.1 is enough when wfb_rx runs on this machine.";
pub const SETTINGS_CODEC_HINT: &str =
    "Auto reads the codec off the stream itself, which is right in practice. Pin one only if detection gets it wrong.";
pub const SETTINGS_VIDEO_HINT: &str = "How the picture is drawn in the window.";
pub const SETTINGS_UI_HINT: &str = "Text size for the whole app.";
pub const SETTINGS_SAVE: &str =
    "Apply these now and write them to the config file. Comments in the file are kept.";

/// What to say when there is no picture.
///
/// Each of these is a different fault with a different fix, and telling them
/// apart from a black window alone is impossible - which is the whole reason
/// the app says anything at all here rather than showing nothing.
pub const NO_VIDEO_TITLE: &str = "No video";

/// Nothing has ever arrived on the socket.
///
/// `remote` asks for the version aimed at a viewer that is not the machine
/// running wfb_rx. It is the more common mistake by far on a phone, because
/// wfb_rx sends to 127.0.0.1 unless told otherwise: the ground station looks
/// completely healthy, the laptop's own view works, and the phone sits on a
/// silent socket with nothing anywhere to say why.
pub fn no_packets(bind: &str, port: u16, remote: bool) -> String {
    let start = format!(
        "Nothing is arriving on {bind}:{port}.\n\n\
         Start the receiver on the ground station:\n\
         sudo ./vrx.sh up 161\n"
    );
    if remote {
        format!(
            "{start}\n\
             Then point wfb_rx at this device - it sends to 127.0.0.1 unless\n\
             given -c, so `./vrx.sh rx` alone never reaches it:\n\
             wfb_rx -p 0 -u {port} -K gs.key -i <link id> \\\n\
             \x20   -c <this device's ip> <interface>\n\n\
             Check too that the channel, key and link id match the air unit, \
             and that both devices are on the same network."
        )
    } else {
        format!(
            "{start}./vrx.sh rx\n\n\
             If wfb_rx is already running, check that it was given -u {port}, \
             and that the channel, key and link id match the air unit."
        )
    }
}

/// Packets were arriving and then stopped.
pub fn packets_stopped(seconds: f64) -> String {
    format!(
        "The stream stopped {seconds:.0} s ago.\n\n\
         The air unit may have lost power or flown out of range. wfb_rx keeps \
         running, so this page will pick the stream back up on its own when it \
         returns."
    )
}

/// Packets are arriving but nothing decodes.
pub const NO_FRAMES: &str = "Packets are arriving but nothing is decoding.\n\n\
     Usually the wrong codec, which the app is re-detecting now. If it persists, \
     the stream may be encrypted with a key this ground station does not have - \
     wfb_rx would report a rising dec_err count.";

/// The source could not be opened at all.
///
/// Two very different failures share this: a socket that would not bind and
/// an adapter that is not there. The message from the source itself is the
/// specific half; what follows it is what to do about it.
pub fn source_failed(reason: &str, radio: bool) -> String {
    if radio {
        format!(
            "{reason}.\n\n\
             Check that the adapter is plugged in and that it is one this \
             build can drive - an RTL8812AU, 8814AU or 8812EU. On Linux, \
             reaching it without root needs a udev rule; on Android, the \
             permission prompt has to be accepted.\n\n\
             If the message is about the key file, copy gs.key from the \
             ground station to the path on the Settings page."
        )
    } else {
        format!(
            "{reason}.\n\n\
             Another copy of this app, or a gst-launch left running, is most \
             likely holding the port. Change the port on the Settings page to \
             use a different one."
        )
    }
}

/// How far a radio link got before it stopped.
///
/// The stages are ordered, and each one has a different fix. A black screen
/// looks identical for all of them, which is the entire reason this exists:
/// on a link that is not working, knowing that frames are arriving but none
/// of them are ours is the difference between a five-second fix and an hour.
///
/// Without the radio feature only the last two are reachable - there is no
/// adapter to be in any of the earlier states - but the prose is kept whole
/// rather than split across a cfg: it reads as one explanation.
#[cfg_attr(not(feature = "radio"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RadioStage {
    /// The adapter is listening and hearing nothing at all.
    Silent,
    /// The channel is busy, but none of it carries our link id.
    NotOurs,
    /// Our frames are arriving, but no session key has been read from them.
    NoSession,
    /// The session is up and packets are being dropped as unreadable.
    NotDecrypting,
    /// Packets are flowing and the decoder is producing nothing.
    NotDecoding,
    /// It was working and has stopped.
    Stopped(f64),
}

/// What to say about a radio link with no picture.
///
/// `key` is named rather than described because a ground station usually has
/// more than one lying about - one per air unit, plus whatever came with the
/// hardware - and "the key is wrong" is not actionable when you cannot see
/// which of them is being read.
pub fn radio_no_video(
    stage: RadioStage,
    channel: u8,
    width: &str,
    link_id: u32,
    key: &str,
) -> String {
    match stage {
        RadioStage::Silent => format!(
            "Listening on channel {channel} at {width}, and the band is \n\
             completely quiet.\n\n\
             Either the air unit is not transmitting, or it is on another \
             channel. Check that it has power, and that the channel here \
             matches the one it was flashed with."
        ),
        RadioStage::NotOurs => format!(
            "Channel {channel} is busy, but none of the traffic is ours.\n\n\
             The adapter and the channel are right; the link id is not. This \
             one is set to {link_id}. In the drone-cam checkout, \n\
             `sudo ./vrx.sh scan` reads the id off the air."
        ),
        RadioStage::NoSession => format!(
            "Our frames are arriving, but none of them opens.\n\n\
             The air unit is transmitting and the link id matches, so what is \
             left is the key. This one:\n\
             {key}\n\
             is not the peer of the drone.key the air unit was flashed with. \
             Point the Settings page at the gs.key that came from the ground \
             station this air unit was paired with."
        ),
        RadioStage::NotDecrypting => {
            "The session is up but the video packets are being rejected.\n\n\
             Usually two air units on one link id, or a key that changed \
             without this end being restarted."
                .to_string()
        }
        RadioStage::NotDecoding => NO_FRAMES.to_string(),
        RadioStage::Stopped(seconds) => format!(
            "The link went quiet {seconds:.0} s ago.\n\n\
             The air unit may have lost power or flown out of range. This \
             page picks the stream back up on its own when it returns."
        ),
    }
}
