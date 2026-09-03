//! The Link page: what the RTP layer sees.
//!
//! Everything here is counted in [`crate::video::rtp`] rather than derived
//! from the picture, which is the point of the page. On this link the picture
//! can look fine while the margin is nearly gone - FEC repairs some loss and
//! the decoder conceals more - so the numbers that show trouble coming are not
//! visible in the video at all.

use crate::app::{DroneApp, SafeArea, HISTORY_S};
use crate::video::Stats;

use super::super::text;
use super::super::theme::{GAP_BLOCK, GAP_SECTION};
use super::super::widgets::{content_page, heading, reading, section, wide_button};
use super::super::{plot, theme::gap};
use super::{bitrate, count, since};

impl DroneApp {
    pub(in crate::app) fn page_link(
        &mut self,
        ctx: &egui::Context,
        screen: egui::Rect,
        safe: SafeArea,
        stats: &Stats,
    ) {
        let settings = self.ui_settings();
        let history = self.history.clone();

        content_page(ctx, "page_link", screen, safe, |ui| {
            heading(ui, "Link");

            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.label(egui::RichText::new(text::LINK_HINT).weak().small());
                gap(ui, GAP_BLOCK);

                plot::link_history(ui, &history, HISTORY_S, settings.ok, settings.error);
                gap(ui, GAP_BLOCK);

                section(ui, "Stream", None);
                reading(
                    ui,
                    "Codec",
                    &stats
                        .codec
                        .map_or_else(|| "detecting".to_string(), |c| c.to_string()),
                    None,
                );
                let size = if stats.width > 0 {
                    format!("{}x{}", stats.width, stats.height)
                } else {
                    "-".to_string()
                };
                reading(ui, "Resolution", &size, None);
                reading(ui, "Frame rate", &format!("{:.1} fps", stats.fps), None);
                reading(ui, "Bit rate", &bitrate(stats.bitrate_bps), None);

                section(ui, "Reception", None);
                reading(ui, "Packets", &count(stats.rtp.packets), None);
                reading(
                    ui,
                    "Packet rate",
                    &format!("{:.0} /s", stats.packet_rate),
                    None,
                );
                // The one reading with a color: it is the one that says
                // whether to keep flying.
                reading(
                    ui,
                    "Lost",
                    &format!("{} ({:.2}%)", count(stats.rtp.lost), stats.rtp.loss_pct()),
                    Some(loss_color(stats.rtp.loss_pct(), &settings)),
                );
                reading(ui, "Reordered", &count(stats.rtp.reordered), None);
                reading(ui, "Stream restarts", &count(stats.rtp.resets), None);
                reading(ui, "Not RTP", &count(stats.rtp.malformed), None);

                section(ui, "Decoding", None);
                reading(ui, "Pictures", &count(stats.rtp.access_units), None);
                reading(ui, "Incomplete", &count(stats.rtp.damaged), None);
                reading(ui, "Decoded", &count(stats.frames), None);
                reading(
                    ui,
                    "Decode errors",
                    &count(stats.decode_errors),
                    (stats.decode_errors > 0).then_some(settings.error),
                );
                // A steady count here means the display is the bottleneck
                // rather than the link, which is worth telling apart from
                // loss - it is the one number on this page that is the app's
                // own fault.
                reading(
                    ui,
                    "Frames dropped by the UI",
                    &count(stats.dropped_frames),
                    None,
                );

                section(ui, "Timing", None);
                reading(ui, "Last packet", &since(stats.since_packet_s), None);
                reading(ui, "Last picture", &since(stats.since_frame_s), None);

                gap(ui, GAP_SECTION);
                if wide_button(ui, "Restart the decoder")
                    .on_hover_text(text::LINK_RESTART)
                    .clicked()
                {
                    self.video.restart();
                }
            });
        });
    }
}

/// The color a loss reading is drawn in.
fn loss_color(loss_pct: f32, settings: &crate::config::UiSettings) -> egui::Color32 {
    if loss_pct > 5.0 {
        settings.error
    } else if loss_pct > 1.0 {
        settings.warn
    } else {
        settings.ok
    }
}
