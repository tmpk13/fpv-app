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
    "Where the RTP stream arrives. These must match what wfb_rx was told to unpack to.";
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

/// The socket could not be bound at all.
pub fn bind_failed(reason: &str, bind: &str, port: u16) -> String {
    format!(
        "Cannot listen on {bind}:{port}: {reason}.\n\n\
         Another copy of this app, or a gst-launch left running, is most likely \
         holding the port. Change the port on the Settings page to use a \
         different one."
    )
}
