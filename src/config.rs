//! Loadable TOML configuration: where the stream arrives, how it is drawn, and
//! the page colors.
//!
//! The Settings page edits these live and writes them back with
//! [`AppConfig::save`], which edits an existing file in place (comments, key
//! order, and keys this app does not know about all survive) and generates a
//! documented one from [`AppConfig::to_toml`] when there is nothing there yet.
//!
//! Schema (all fields optional; missing ones keep their defaults):
//!
//! ```toml
//! [source]
//! kind = "radio"      # "radio" to drive the adapter here, "udp" to take
//!                     # RTP another machine's wfb_rx already unpacked
//! codec = "auto"      # "auto", "h264" or "h265"
//!
//! [source.radio]
//! channel = 161       # must match the air unit
//! bandwidth = 20      # 20 or 40 MHz
//! link_id = 7669206   # must match the air unit's `wfb_tx -i`
//! radio_port = 0      # the wfb-ng port carrying video
//! key = "gs.key"      # relative paths are resolved beside this file
//!
//! [source.udp]
//! bind = "0.0.0.0"    # "0.0.0.0" to receive from the network, "127.0.0.1"
//!                     # when wfb_rx runs on this machine
//! port = 5600         # the port drone-cam's `wfb_rx -u` unpacks video to
//!
//! [video]
//! fill = false        # crop to fill the window rather than fitting inside it
//! overlay = true      # draw the fps/bitrate readout over the picture
//! smooth = true       # bilinear scaling; false gives nearest-neighbor
//!
//! [ui]
//! ok = "#3cb44b"      # the green feedback lines
//! error = "#dc503c"   # errors and the red feedback lines
//! warn = "#e6a020"    # a link that is degraded but still up
//! background = ""     # pages and popups; empty follows the theme
//! text = ""           # body text; empty follows the theme
//! text_scale = 1.0    # multiplier on the page text; 1.0 is the default size
//! ```

use std::path::{Path, PathBuf};

use egui::Color32;
use serde::Deserialize;
use toml_edit::{DocumentMut, Item, Table, Value};

use crate::video::{Bandwidth, Codec, RadioSettings, Source, SourceKind, UdpSettings};

/// The file the app reads and the Settings page writes.
pub const CONFIG_FILE: &str = "drone-app.toml";

/// Colors and sizing for the pages.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UiSettings {
    pub ok: Color32,
    pub error: Color32,
    /// A link that is up but losing packets: neither of the other two.
    pub warn: Color32,
    /// Empty means "follow the egui theme", which is why these are options
    /// rather than colors with a default. A default would silently override
    /// the theme for anyone who never set one.
    pub background: Option<Color32>,
    pub text: Option<Color32>,
    /// Multiplier on every text size. The whole UI is measured in body-text
    /// heights, so this scales the layout with the type.
    pub text_scale: f32,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            ok: Color32::from_rgb(60, 178, 75),
            error: Color32::from_rgb(220, 80, 60),
            warn: Color32::from_rgb(230, 160, 32),
            background: None,
            text: None,
            text_scale: 1.0,
        }
    }
}

/// How the picture is drawn in the window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VideoSettings {
    /// Crop to fill rather than letterbox to fit.
    pub fill: bool,
    /// Draw the fps and bitrate readout over the picture.
    pub overlay: bool,
    /// Bilinear rather than nearest-neighbor scaling.
    pub smooth: bool,
}

impl Default for VideoSettings {
    fn default() -> Self {
        Self {
            fill: false,
            overlay: true,
            smooth: true,
        }
    }
}

/// Everything the config file holds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AppConfig {
    pub source: Source,
    pub video: VideoSettings,
    pub ui: UiSettings,
    /// Where this was loaded from, and where [`AppConfig::save`] writes.
    /// `None` when no path was available, which is what makes saving a no-op
    /// rather than an error.
    pub path: Option<PathBuf>,
}

/// The wire format, kept apart from [`AppConfig`] so the app's own types stay
/// free of serde's shape (every field optional, colors as strings).
#[derive(Debug, Default, Deserialize)]
struct RawConfig {
    #[serde(default)]
    source: RawSource,
    #[serde(default)]
    video: RawVideo,
    #[serde(default)]
    ui: RawUi,
}

#[derive(Debug, Default, Deserialize)]
struct RawSource {
    kind: Option<String>,
    codec: Option<String>,
    #[serde(default)]
    radio: RawRadio,
    #[serde(default)]
    udp: RawUdp,
    // Accepted where `[source.udp]` now lives, so a config written by the
    // version before the radio existed still works unchanged.
    bind: Option<String>,
    port: Option<u16>,
}

#[derive(Debug, Default, Deserialize)]
struct RawRadio {
    channel: Option<u8>,
    bandwidth: Option<u32>,
    link_id: Option<u32>,
    radio_port: Option<u8>,
    key: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawUdp {
    bind: Option<String>,
    port: Option<u16>,
}

#[derive(Debug, Default, Deserialize)]
struct RawVideo {
    fill: Option<bool>,
    overlay: Option<bool>,
    smooth: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
struct RawUi {
    ok: Option<String>,
    error: Option<String>,
    warn: Option<String>,
    background: Option<String>,
    text: Option<String>,
    text_scale: Option<f32>,
}

impl AppConfig {
    /// Read the config at `path`, falling back to defaults.
    ///
    /// A missing file is not an error - it is the normal first run - and
    /// neither is a malformed one: the app starts on defaults with a warning
    /// in the log rather than refusing to open. Losing the video feed because
    /// of a typo in a color would be a poor trade.
    pub fn load(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref();
        let mut config = match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str::<RawConfig>(&text) {
                Ok(raw) => raw.into_config(),
                Err(err) => {
                    log::warn!("{}: {err}; using defaults", path.display());
                    Self::default()
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(err) => {
                log::warn!("{}: {err}; using defaults", path.display());
                Self::default()
            }
        };
        config.path = Some(path.to_path_buf());
        config
    }

    /// Write the config back, preserving whatever else is in the file.
    ///
    /// An existing file is edited key by key through `toml_edit`, so comments,
    /// key order and any section this build does not know about all survive a
    /// round trip. Only when there is no file yet is one generated from
    /// [`AppConfig::to_toml`], which is where the documented template lives.
    pub fn save(&self) -> Result<(), String> {
        let Some(path) = self.path.as_ref() else {
            return Err("no config path to save to".into());
        };

        let mut doc = match std::fs::read_to_string(path) {
            Ok(text) => text
                .parse::<DocumentMut>()
                .map_err(|err| format!("{}: {err}", path.display()))?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return std::fs::write(path, self.to_toml())
                    .map_err(|err| format!("{}: {err}", path.display()));
            }
            Err(err) => return Err(format!("{}: {err}", path.display())),
        };

        self.write_into(&mut doc);
        std::fs::write(path, doc.to_string()).map_err(|err| format!("{}: {err}", path.display()))
    }

    /// Apply this config's values to an existing document.
    fn write_into(&self, doc: &mut DocumentMut) {
        let source = section(doc, "source");
        source["kind"] = value(self.source.kind.as_str().to_string());
        source["codec"] = value(codec_name(self.source.codec).to_string());

        let radio = subsection(doc, "source", "radio");
        radio["channel"] = value(i64::from(self.source.radio.channel));
        radio["bandwidth"] = value(i64::from(self.source.radio.bandwidth.mhz()));
        radio["link_id"] = value(i64::from(self.source.radio.link_id));
        radio["radio_port"] = value(i64::from(self.source.radio.radio_port));
        radio["key"] = value(self.source.radio.key_path.display().to_string());

        let udp = subsection(doc, "source", "udp");
        udp["bind"] = value(self.source.udp.bind.to_string());
        udp["port"] = value(i64::from(self.source.udp.port));

        let video = section(doc, "video");
        video["fill"] = value(self.video.fill);
        video["overlay"] = value(self.video.overlay);
        video["smooth"] = value(self.video.smooth);

        let ui = section(doc, "ui");
        ui["ok"] = value(to_hex(self.ui.ok));
        ui["error"] = value(to_hex(self.ui.error));
        ui["warn"] = value(to_hex(self.ui.warn));
        ui["background"] = value(self.ui.background.map(to_hex).unwrap_or_default());
        ui["text"] = value(self.ui.text.map(to_hex).unwrap_or_default());
        ui["text_scale"] = value(f64::from(self.ui.text_scale));
    }

    /// A documented config file holding this config's values.
    ///
    /// Written only when there is no file yet, so a first save leaves
    /// something worth opening in an editor rather than a bare dump of keys.
    pub fn to_toml(&self) -> String {
        format!(
            "# drone-app settings. The Settings page writes this file back in\n\
             # place, so comments and key order here survive being saved.\n\
             \n\
             [source]\n\
             # \"radio\" drives the adapter on this machine and receives the\n\
             # link itself. \"udp\" takes RTP another machine's wfb_rx has\n\
             # already decrypted and forwarded.\n\
             kind = \"{kind}\"\n\
             # \"auto\", \"h264\" or \"h265\". Auto reads it off the stream.\n\
             codec = \"{codec}\"\n\
             \n\
             [source.radio]\n\
             # All four must match the air unit. `sudo ./vrx.sh scan` in the\n\
             # drone-cam checkout reads the channel and link id off the air.\n\
             channel = {channel}\n\
             bandwidth = {bandwidth}\n\
             link_id = {link_id}\n\
             radio_port = {radio_port}\n\
             # The ground station half of the wfb_keygen pair. A relative path\n\
             # is resolved beside this file.\n\
             key = \"{key}\"\n\
             \n\
             [source.udp]\n\
             # \"0.0.0.0\" receives from anywhere on the network, which is what\n\
             # a phone needs. \"127.0.0.1\" is enough when wfb_rx runs here.\n\
             bind = \"{bind}\"\n\
             # The port drone-cam's `wfb_rx -u` unpacks video to.\n\
             port = {port}\n\
             \n\
             [video]\n\
             # Crop to fill the window rather than fitting the whole picture.\n\
             fill = {fill}\n\
             # The fps and bitrate readout over the picture.\n\
             overlay = {overlay}\n\
             # Bilinear scaling; false gives nearest-neighbor.\n\
             smooth = {smooth}\n\
             \n\
             [ui]\n\
             ok = \"{ok}\"\n\
             error = \"{error}\"\n\
             warn = \"{warn}\"\n\
             # Empty follows the egui theme.\n\
             background = \"{background}\"\n\
             text = \"{text}\"\n\
             # Multiplier on the page text; 1.0 is the default size.\n\
             text_scale = {text_scale}\n",
            kind = self.source.kind.as_str(),
            codec = codec_name(self.source.codec),
            channel = self.source.radio.channel,
            bandwidth = self.source.radio.bandwidth.mhz(),
            link_id = self.source.radio.link_id,
            radio_port = self.source.radio.radio_port,
            key = self.source.radio.key_path.display(),
            bind = self.source.udp.bind,
            port = self.source.udp.port,
            fill = self.video.fill,
            overlay = self.video.overlay,
            smooth = self.video.smooth,
            ok = to_hex(self.ui.ok),
            error = to_hex(self.ui.error),
            warn = to_hex(self.ui.warn),
            background = self.ui.background.map(to_hex).unwrap_or_default(),
            text = self.ui.text.map(to_hex).unwrap_or_default(),
            text_scale = self.ui.text_scale,
        )
    }

    /// Resolve a relative key file against `base`.
    ///
    /// Where `base` is depends on the platform and only the caller knows it.
    /// On a desktop it is the config file's own directory, so `gs.key` beside
    /// `drone-app.toml` works and the pair travel together. On Android it is
    /// the app's external files directory, because that is the only place a
    /// file can be put from outside the app without a storage permission, and
    /// it is nowhere near the config.
    ///
    /// An absolute path is left alone: someone who wrote one meant it.
    pub fn resolve_key_path(&mut self, base: impl AsRef<Path>) {
        if self.source.radio.key_path.is_relative() {
            self.source.radio.key_path = base.as_ref().join(&self.source.radio.key_path);
        }
    }
}

impl RawConfig {
    fn into_config(self) -> AppConfig {
        let defaults = AppConfig::default();
        AppConfig {
            source: Source {
                kind: match self.source.kind.as_deref().map(str::trim) {
                    Some("udp") => SourceKind::Udp,
                    Some("radio") => SourceKind::Radio,
                    // Anything unrecognized, including nothing at all, drives
                    // the radio: that is what the app is for, and a config
                    // written before the radio existed named no kind.
                    _ => defaults.source.kind,
                },
                radio: RadioSettings {
                    channel: self
                        .source
                        .radio
                        .channel
                        .filter(|c| *c > 0)
                        .unwrap_or(defaults.source.radio.channel),
                    bandwidth: self
                        .source
                        .radio
                        .bandwidth
                        .map(Bandwidth::from_mhz)
                        .unwrap_or(defaults.source.radio.bandwidth),
                    link_id: self
                        .source
                        .radio
                        .link_id
                        .unwrap_or(defaults.source.radio.link_id),
                    radio_port: self
                        .source
                        .radio
                        .radio_port
                        .unwrap_or(defaults.source.radio.radio_port),
                    key_path: self
                        .source
                        .radio
                        .key
                        .map(PathBuf::from)
                        .unwrap_or(defaults.source.radio.key_path),
                },
                udp: UdpSettings {
                    // The pre-radio schema put these directly under [source];
                    // that spelling still wins if the newer one is absent, so
                    // an existing config file keeps working.
                    bind: self
                        .source
                        .udp
                        .bind
                        .or(self.source.bind)
                        .and_then(|s| s.trim().parse().ok())
                        .unwrap_or(defaults.source.udp.bind),
                    port: self
                        .source
                        .udp
                        .port
                        .or(self.source.port)
                        .unwrap_or(defaults.source.udp.port),
                },
                codec: self.source.codec.as_deref().and_then(parse_codec),
            },
            video: VideoSettings {
                fill: self.video.fill.unwrap_or(defaults.video.fill),
                overlay: self.video.overlay.unwrap_or(defaults.video.overlay),
                smooth: self.video.smooth.unwrap_or(defaults.video.smooth),
            },
            ui: UiSettings {
                ok: self
                    .ui
                    .ok
                    .as_deref()
                    .and_then(parse_color)
                    .unwrap_or(defaults.ui.ok),
                error: self
                    .ui
                    .error
                    .as_deref()
                    .and_then(parse_color)
                    .unwrap_or(defaults.ui.error),
                warn: self
                    .ui
                    .warn
                    .as_deref()
                    .and_then(parse_color)
                    .unwrap_or(defaults.ui.warn),
                background: self.ui.background.as_deref().and_then(parse_color),
                text: self.ui.text.as_deref().and_then(parse_color),
                // A zero or negative scale would make the text vanish with no
                // way back through the UI that set it.
                text_scale: self
                    .ui
                    .text_scale
                    .filter(|s| *s > 0.0)
                    .unwrap_or(defaults.ui.text_scale)
                    .clamp(0.5, 3.0),
            },
            path: None,
        }
    }
}

/// The config name for a codec setting. `None` is the detector.
fn codec_name(codec: Option<Codec>) -> &'static str {
    match codec {
        None => "auto",
        Some(Codec::H264) => "h264",
        Some(Codec::H265) => "h265",
    }
}

/// Parse a codec setting. Anything unrecognized, "auto" included, means
/// detect.
fn parse_codec(text: &str) -> Option<Codec> {
    match text.trim().to_ascii_lowercase().as_str() {
        "h264" | "avc" => Some(Codec::H264),
        "h265" | "hevc" => Some(Codec::H265),
        _ => None,
    }
}

/// Parse `#rrggbb` or `rrggbb`. An empty string means "follow the theme", so
/// it parses to `None` rather than to an error.
fn parse_color(text: &str) -> Option<Color32> {
    let hex = text.trim().trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    let value = u32::from_str_radix(hex, 16).ok()?;
    Some(Color32::from_rgb(
        (value >> 16) as u8,
        (value >> 8) as u8,
        value as u8,
    ))
}

fn to_hex(color: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", color.r(), color.g(), color.b())
}

/// Fetch or create a table in the document, keeping it out of the inline
/// syntax so it reads as a `[section]` header.
fn section<'a>(doc: &'a mut DocumentMut, name: &str) -> &'a mut Item {
    if !doc.contains_key(name) {
        doc[name] = Item::Table(Table::new());
    }
    &mut doc[name]
}

/// A `[parent.child]` table, created if it is not there.
///
/// Written as a nested table rather than a dotted key so a hand-edited file
/// that already spells it `[source.radio]` is updated in place instead of
/// gaining a second copy of every key.
fn subsection<'a>(doc: &'a mut DocumentMut, parent: &str, name: &str) -> &'a mut Item {
    let parent = section(doc, parent);
    if parent.get(name).is_none() {
        parent[name] = Item::Table(Table::new());
    }
    &mut parent[name]
}

/// Wrap a value so it can be assigned into a document.
fn value(v: impl Into<Value>) -> Item {
    Item::Value(v.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn an_empty_document_gives_the_defaults() {
        let config = toml::from_str::<RawConfig>("").unwrap().into_config();
        assert_eq!(config.source, Source::default());
        assert_eq!(config.video, VideoSettings::default());
        assert_eq!(config.ui, UiSettings::default());
    }

    #[test]
    fn reads_a_full_document() {
        let text = r##"
            [source]
            kind = "udp"
            codec = "h264"

            [source.radio]
            channel = 149
            bandwidth = 40
            link_id = 12345
            radio_port = 2
            key = "/tmp/gs.key"

            [source.udp]
            bind = "127.0.0.1"
            port = 5602

            [video]
            fill = true
            overlay = false
            smooth = false

            [ui]
            ok = "#010203"
            text_scale = 1.5
        "##;
        let config = toml::from_str::<RawConfig>(text).unwrap().into_config();
        assert_eq!(config.source.kind, SourceKind::Udp);
        assert_eq!(config.source.udp.bind, Ipv4Addr::LOCALHOST);
        assert_eq!(config.source.udp.port, 5602);
        assert_eq!(config.source.radio.channel, 149);
        assert_eq!(config.source.radio.bandwidth, Bandwidth::Mhz40);
        assert_eq!(config.source.radio.link_id, 12345);
        assert_eq!(config.source.radio.radio_port, 2);
        assert_eq!(config.source.radio.key_path, PathBuf::from("/tmp/gs.key"));
        assert_eq!(config.source.codec, Some(Codec::H264));
        assert!(config.video.fill);
        assert!(!config.video.overlay);
        assert!(!config.video.smooth);
        assert_eq!(config.ui.ok, Color32::from_rgb(1, 2, 3));
        assert_eq!(config.ui.text_scale, 1.5);
    }

    #[test]
    fn a_partial_section_keeps_the_other_defaults() {
        let config = toml::from_str::<RawConfig>("[video]\nfill = true\n")
            .unwrap()
            .into_config();
        assert!(config.video.fill);
        assert_eq!(
            config.video.overlay,
            VideoSettings::default().overlay,
            "an unset key must not be read as false"
        );
    }

    #[test]
    fn codec_names_round_trip() {
        for (text, expected) in [
            ("auto", None),
            ("h264", Some(Codec::H264)),
            ("H265", Some(Codec::H265)),
            ("hevc", Some(Codec::H265)),
            ("avc", Some(Codec::H264)),
            ("nonsense", None),
        ] {
            assert_eq!(parse_codec(text), expected, "{text}");
        }
        assert_eq!(
            parse_codec(codec_name(Some(Codec::H265))),
            Some(Codec::H265)
        );
        assert_eq!(parse_codec(codec_name(None)), None);
    }

    #[test]
    fn colors_round_trip_and_reject_junk() {
        let color = Color32::from_rgb(0x12, 0x34, 0x56);
        assert_eq!(to_hex(color), "#123456");
        assert_eq!(parse_color("#123456"), Some(color));
        assert_eq!(parse_color("123456"), Some(color));
        assert_eq!(parse_color(""), None, "empty means follow the theme");
        assert_eq!(parse_color("#12345"), None);
        assert_eq!(parse_color("#gggggg"), None);
    }

    #[test]
    fn a_bad_text_scale_cannot_make_the_text_vanish() {
        for bad in ["0.0", "-3.0"] {
            let config = toml::from_str::<RawConfig>(&format!("[ui]\ntext_scale = {bad}"))
                .unwrap()
                .into_config();
            assert_eq!(config.ui.text_scale, 1.0, "{bad}");
        }
        // An enormous one is clamped rather than rejected.
        let config = toml::from_str::<RawConfig>("[ui]\ntext_scale = 99.0")
            .unwrap()
            .into_config();
        assert_eq!(config.ui.text_scale, 3.0);
    }

    #[test]
    fn a_malformed_bind_address_falls_back_to_the_default() {
        let config = toml::from_str::<RawConfig>("[source.udp]\nbind = \"not an address\"")
            .unwrap()
            .into_config();
        assert_eq!(config.source.udp.bind, Source::default().udp.bind);
    }

    #[test]
    fn a_config_from_before_the_radio_still_works() {
        // The old schema put bind and port directly under [source] and had no
        // kind at all. Reading one of those must not silently move the app to
        // a port it was never told about.
        let text = "[source]\nbind = \"127.0.0.1\"\nport = 5605\ncodec = \"h265\"\n";
        let config = toml::from_str::<RawConfig>(text).unwrap().into_config();
        assert_eq!(config.source.udp.bind, Ipv4Addr::LOCALHOST);
        assert_eq!(config.source.udp.port, 5605);
        assert_eq!(config.source.codec, Some(Codec::H265));
        // With no kind named, the radio is what the app is for.
        assert_eq!(config.source.kind, SourceKind::Radio);
    }

    #[test]
    fn a_relative_key_file_is_resolved_against_the_base_given() {
        let mut config = AppConfig::default();
        config.source.radio.key_path = PathBuf::from("gs.key");
        config.resolve_key_path("/etc/drone");
        assert_eq!(
            config.source.radio.key_path,
            PathBuf::from("/etc/drone/gs.key"),
            "a key file named beside the config must be found beside it"
        );

        // An absolute path is left alone, and resolving twice is a no-op -
        // which matters because the Android path calls this after load.
        config.resolve_key_path("/somewhere/else");
        assert_eq!(
            config.source.radio.key_path,
            PathBuf::from("/etc/drone/gs.key")
        );
    }

    #[test]
    fn saving_preserves_comments_and_unknown_keys() {
        let original = "# keep me\n\
                        [source]\n\
                        port = 5600\n\
                        # this one too\n\
                        codec = \"auto\"\n\
                        \n\
                        [something_else]\n\
                        from_a_newer_build = 7\n";
        let mut doc = original.parse::<DocumentMut>().unwrap();

        let mut config = AppConfig::default();
        config.source.udp.port = 5700;
        config.write_into(&mut doc);

        let saved = doc.to_string();
        assert!(saved.contains("# keep me"), "a comment was lost:\n{saved}");
        assert!(saved.contains("# this one too"));
        assert!(
            saved.contains("from_a_newer_build = 7"),
            "a key this build does not know was dropped:\n{saved}"
        );
        assert!(saved.contains("port = 5700"), "the edit did not apply");
    }

    #[test]
    fn the_generated_template_parses_back_to_what_wrote_it() {
        let mut config = AppConfig::default();
        config.source.udp.port = 5601;
        config.source.radio.channel = 149;
        config.source.radio.link_id = 424242;
        config.source.codec = Some(Codec::H265);
        config.video.fill = true;
        config.ui.text_scale = 1.25;

        let round_tripped = toml::from_str::<RawConfig>(&config.to_toml())
            .expect("the generated template must be valid TOML")
            .into_config();

        assert_eq!(round_tripped.source, config.source);
        assert_eq!(round_tripped.video, config.video);
        assert_eq!(round_tripped.ui, config.ui);
    }

    #[test]
    fn an_unreadable_file_starts_the_app_on_defaults() {
        // A directory, which is present but cannot be read as a file.
        let config = AppConfig::load(std::env::temp_dir());
        assert_eq!(config.source, Source::default());
        assert!(
            config.path.is_some(),
            "the path is kept so a save can fix it"
        );
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        let path = std::env::temp_dir().join("drone-app-does-not-exist.toml");
        let config = AppConfig::load(&path);
        assert_eq!(config.source, Source::default());
        assert_eq!(config.path.as_deref(), Some(path.as_path()));
    }
}
