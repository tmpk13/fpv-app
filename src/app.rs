//! The app: what it holds, and what happens each frame.
//!
//! State and the frame loop live here; everything that draws lives in [`ui`].
//! The split is the one gps-gui-rs uses, for the same reason - a page's
//! definition should read as a list of what is on it, without the egui
//! scaffolding around it.

use std::time::Instant;

use crate::config::{AppConfig, UiSettings};
use crate::video::{
    Bandwidth, Codec, Frame, RadioSettings, Source, SourceKind, Stats, UdpSettings, VideoHandle,
};

/// The view layer. Kept in a submodule so this file holds only state and the
/// core update logic; the `impl DroneApp` blocks there render each page.
mod ui;

/// The pages, in menu order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Page {
    /// The picture, full screen.
    Video,
    /// What the link is doing.
    Link,
    Settings,
    /// The navigation itself.
    Menu,
}

/// Screen edges that are covered by system decoration, in points.
///
/// Zero on desktop. On Android these are the status bar, the gesture bar and,
/// in landscape, the display cutout - all of which a full-screen video happily
/// paints under, and none of which a control may sit under.
#[derive(Clone, Copy, Debug, Default)]
pub struct SafeArea {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

/// A history sample for the Link page's graph.
#[derive(Clone, Copy)]
struct Sample {
    at: Instant,
    bitrate_bps: f64,
    loss_pct: f32,
}

/// How much link history the graph keeps, in seconds.
const HISTORY_S: f64 = 60.0;

/// How often a sample is taken. Fast enough to show a dropout, slow enough
/// that a minute of history is a few hundred points rather than a few
/// thousand.
const SAMPLE_INTERVAL_S: f64 = 0.25;

pub struct DroneApp {
    pub(crate) config: AppConfig,
    pub(crate) video: VideoHandle,

    /// The current page, and the one the menu was opened from, so closing it
    /// without choosing goes back rather than to a default.
    pub(crate) page: Page,
    pub(crate) menu_from: Page,

    /// The texture the decoded picture is uploaded to, reused across frames.
    /// `None` until the first picture arrives.
    texture: Option<egui::TextureHandle>,
    /// Size of the picture currently in `texture`, so a resolution change can
    /// be told from a redraw at the same size.
    texture_size: [usize; 2],

    /// Link history for the graph, oldest first.
    history: Vec<Sample>,
    last_sample: Option<Instant>,

    /// Where the system decoration is, queried each frame because it changes
    /// on rotation.
    insets: Option<Box<dyn Fn() -> [f32; 4]>>,

    /// The Settings page's working copy, applied on save rather than on every
    /// keystroke - a port edited digit by digit would otherwise rebind the
    /// socket to a nonsense port on the way to a real one.
    pub(crate) draft: Draft,
    /// The result of the last save.
    pub(crate) save_result: Option<Result<String, String>>,

    /// What the style was last written for, so it is only rewritten when one
    /// of those inputs changes rather than on every frame.
    pub(in crate::app) style_applied: Option<ui::AppliedStyle>,
}

/// The Settings page's editable copy of the config.
///
/// The numbers are strings because they are being typed: a partially typed
/// port is not a number, and forcing it through one as it is entered either
/// rejects the keystroke or silently rewrites it.
pub struct Draft {
    pub kind: SourceKind,
    /// Radio.
    pub channel: String,
    pub bandwidth: Bandwidth,
    pub link_id: String,
    pub radio_port: String,
    pub key_path: String,
    /// Forwarded RTP.
    pub bind: String,
    pub port: String,
    /// Both.
    pub codec: Option<Codec>,
    pub fill: bool,
    pub overlay: bool,
    pub smooth: bool,
    pub text_scale: f32,
}

impl Draft {
    fn from_config(config: &AppConfig) -> Self {
        Self {
            kind: config.source.kind,
            channel: config.source.radio.channel.to_string(),
            bandwidth: config.source.radio.bandwidth,
            link_id: config.source.radio.link_id.to_string(),
            radio_port: config.source.radio.radio_port.to_string(),
            key_path: config.source.radio.key_path.display().to_string(),
            bind: config.source.udp.bind.to_string(),
            port: config.source.udp.port.to_string(),
            codec: config.source.codec,
            fill: config.video.fill,
            overlay: config.video.overlay,
            smooth: config.video.smooth,
            text_scale: config.ui.text_scale,
        }
    }

    /// The source this draft describes, or an explanation of why it is not
    /// one yet.
    ///
    /// Both halves are parsed whichever is selected, so switching to the
    /// other one does not turn out to have kept an unusable value.
    pub(crate) fn to_source(&self) -> Result<Source, String> {
        let bind = self
            .bind
            .trim()
            .parse()
            .map_err(|_| format!("\"{}\" is not an IPv4 address", self.bind.trim()))?;
        let port = self
            .port
            .trim()
            .parse::<u16>()
            .map_err(|_| format!("\"{}\" is not a port number", self.port.trim()))?;
        if port == 0 {
            return Err("port 0 asks the system to choose one, which nothing can send to".into());
        }

        let channel = self
            .channel
            .trim()
            .parse::<u8>()
            .ok()
            .filter(|c| *c > 0)
            .ok_or_else(|| format!("\"{}\" is not a channel number", self.channel.trim()))?;
        let link_id = self
            .link_id
            .trim()
            .parse::<u32>()
            .map_err(|_| format!("\"{}\" is not a link id", self.link_id.trim()))?;
        // The link id is 24 bits: the channel id it goes into is that plus an
        // 8-bit radio port. A larger number would silently lose its top bits
        // and match nothing, with every frame discarded and no explanation.
        if link_id > 0x00ff_ffff {
            return Err("a link id is at most 16777215".into());
        }
        let radio_port = self
            .radio_port
            .trim()
            .parse::<u8>()
            .map_err(|_| format!("\"{}\" is not a radio port", self.radio_port.trim()))?;
        if self.key_path.trim().is_empty() {
            return Err("a key file is needed to decrypt the link".into());
        }

        Ok(Source {
            kind: self.kind,
            radio: RadioSettings {
                channel,
                bandwidth: self.bandwidth,
                link_id,
                radio_port,
                key_path: std::path::PathBuf::from(self.key_path.trim()),
            },
            udp: UdpSettings { bind, port },
            codec: self.codec,
        })
    }
}

impl DroneApp {
    pub fn new(
        ctx: egui::Context,
        config: AppConfig,
        insets: Option<Box<dyn Fn() -> [f32; 4]>>,
    ) -> Self {
        let video = crate::video::spawn(ctx, config.source.clone());
        let draft = Draft::from_config(&config);
        Self {
            config,
            video,
            page: Page::Video,
            menu_from: Page::Video,
            texture: None,
            texture_size: [0, 0],
            history: Vec::new(),
            last_sample: None,
            insets,
            draft,
            save_result: None,
            style_applied: None,
        }
    }

    /// The safe area for this frame, in points.
    ///
    /// Queried every frame rather than cached: on Android it changes when the
    /// device rotates, and a cached inset would leave the controls under the
    /// status bar until the app was restarted.
    fn safe_area(&self, ctx: &egui::Context) -> SafeArea {
        let Some(insets) = self.insets.as_ref() else {
            return SafeArea::default();
        };
        // The callback reports physical pixels; the UI is laid out in points.
        let scale = ctx.pixels_per_point().max(0.01);
        let [top, right, bottom, left] = insets();
        SafeArea {
            top: top / scale,
            right: right / scale,
            bottom: bottom / scale,
            left: left / scale,
        }
    }

    /// Upload the newest decoded picture, if one has arrived.
    fn take_frame(&mut self, ctx: &egui::Context) {
        let Some(Frame {
            width,
            height,
            rgba,
        }) = self.video.take_frame()
        else {
            return;
        };
        let size = [width as usize, height as usize];
        if size[0] * size[1] * 4 != rgba.len() {
            log::warn!(
                "dropping a {}x{} frame of {} bytes",
                width,
                height,
                rgba.len()
            );
            return;
        }

        let image = egui::ColorImage::from_rgba_unmultiplied(size, &rgba);
        let options = self.texture_options();

        match self.texture.as_mut() {
            // Reusing the handle uploads into the existing texture rather than
            // allocating a new one every frame, which at 60 fps matters.
            Some(texture) if self.texture_size == size => texture.set(image, options),
            _ => {
                self.texture = Some(ctx.load_texture("video", image, options));
                self.texture_size = size;
            }
        }
    }

    /// How the picture is sampled when it does not land on exact pixels.
    fn texture_options(&self) -> egui::TextureOptions {
        if self.config.video.smooth {
            egui::TextureOptions::LINEAR
        } else {
            // Nearest-neighbor: on a low-resolution stream blown up to a big
            // window, sharp blocks read as more detailed than a blur does.
            egui::TextureOptions::NEAREST
        }
    }

    /// Record a link sample for the graph, at most every
    /// [`SAMPLE_INTERVAL_S`].
    fn sample_history(&mut self, stats: &Stats) {
        let now = Instant::now();
        let due = self
            .last_sample
            .is_none_or(|t| now.duration_since(t).as_secs_f64() >= SAMPLE_INTERVAL_S);
        if !due {
            return;
        }
        self.last_sample = Some(now);
        self.history.push(Sample {
            at: now,
            bitrate_bps: stats.bitrate_bps,
            loss_pct: stats.rtp.loss_pct(),
        });
        self.history
            .retain(|s| now.duration_since(s.at).as_secs_f64() <= HISTORY_S);
    }

    /// Apply the Settings draft to the running app.
    pub(crate) fn apply_draft(&mut self) {
        let source = match self.draft.to_source() {
            Ok(source) => source,
            Err(err) => {
                self.save_result = Some(Err(err));
                return;
            }
        };

        let listening = match source.kind {
            SourceKind::Radio => format!(
                "Receiving on channel {} {}, link {}",
                source.radio.channel,
                source.radio.bandwidth.as_str(),
                source.radio.link_id
            ),
            SourceKind::Udp => format!("Listening on {}:{}", source.udp.bind, source.udp.port),
        };

        self.config.source = source.clone();
        self.config.video.fill = self.draft.fill;
        self.config.video.overlay = self.draft.overlay;
        self.config.video.smooth = self.draft.smooth;
        self.config.ui.text_scale = self.draft.text_scale;

        // Retune before saving: the change to the link is what the user is
        // waiting to see, and it should not be held up by a file that may not
        // be writable.
        self.video.retune(source);
        // A texture built with the old sampling stays as it was until it is
        // replaced, so drop it and let the next frame rebuild it.
        self.texture = None;

        self.save_result = Some(match self.config.save() {
            Ok(()) => Ok(format!("Saved. {listening}")),
            Err(err) => Err(format!("Applied, but not saved: {err}")),
        });
    }

    pub(crate) fn ui_settings(&self) -> UiSettings {
        self.config.ui
    }
}

impl eframe::App for DroneApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let ctx = &ctx;

        self.take_frame(ctx);
        let stats = self.video.stats();
        self.sample_history(&stats);

        // Before anything is drawn: the pages read these colors and text sizes
        // out of the style rather than being handed them.
        self.apply_ui_style(ctx);

        let screen = ctx.input(|i| i.viewport_rect());
        let safe = self.safe_area(ctx);

        match self.page {
            Page::Video => self.page_video(ctx, screen, safe, &stats),
            Page::Link => self.page_link(ctx, screen, safe, &stats),
            Page::Settings => self.page_settings(ctx, screen, safe),
            Page::Menu => self.page_menu_page(ctx, screen, safe),
        }

        // The corner toggle sits over every page including the menu itself,
        // where it is what leaves without choosing.
        self.corner_toggle(ctx, screen, safe);

        // Nothing else asks for a repaint while the link is quiet, and the
        // "seconds since" readouts have to keep counting up: a frozen "0.4 s
        // ago" is worse than no reading at all, because it reads as live.
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
}
