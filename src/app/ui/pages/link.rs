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

                self.radio_readings(ui, stats, &settings);

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
                // The most damaging loss on the page, and the least obvious:
                // a dropped access unit corrupts every picture after it until
                // the next keyframe, so any number here explains artifacts
                // that none of the other counters would.
                reading(
                    ui,
                    "Dropped before decoding",
                    &count(stats.units_dropped),
                    (stats.units_dropped > 0).then_some(settings.error),
                );
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

    /// The radio and wfb-ng sections, when this device is the ground station.
    ///
    /// Absent on a forwarded stream, because then none of it is knowable
    /// here: the machine running `wfb_rx` has the radio, and everything on
    /// this page below is what survived being sent on from it.
    #[cfg(feature = "radio")]
    fn radio_readings(
        &self,
        ui: &mut egui::Ui,
        stats: &Stats,
        settings: &crate::config::UiSettings,
    ) {
        let Some(radio) = stats.radio.as_ref() else {
            return;
        };
        let link = &radio.link;

        section(ui, "Radio", None);
        reading(ui, "Adapter", &radio.chip, None);
        reading(
            ui,
            "Signal",
            &radio
                .signal
                .rssi_dbm
                .map_or_else(|| "-".to_string(), |dbm| format!("{dbm} dBm")),
            radio.signal.rssi_dbm.map(|dbm| signal_color(dbm, settings)),
        );
        reading(
            ui,
            "Signal to noise",
            &radio
                .signal
                .snr_db
                .map_or_else(|| "-".to_string(), |db| format!("{db} dB")),
            None,
        );
        reading(
            ui,
            "Noise floor",
            &radio
                .signal
                .noise_dbm
                .map_or_else(|| "-".to_string(), |dbm| format!("{dbm} dBm")),
            None,
        );
        reading(ui, "Antennas", &antennas(&radio.signal), None);
        // The two frame counts together are the diagnostic: traffic with none
        // of it ours is a link id that does not match, and no traffic at all
        // is the wrong channel.
        reading(ui, "Frames heard", &count(link.total_frames), None);
        reading(
            ui,
            "Frames ours",
            &count(link.frames),
            (link.total_frames > 0 && link.frames == 0).then_some(settings.error),
        );
        reading(ui, "Bad checksum", &count(link.crc_errors), None);

        section(ui, "wfb-ng", None);
        reading(
            ui,
            "Session",
            &if link.has_session() {
                format!("FEC {} of {}, epoch {}", link.fec_k, link.fec_n, link.epoch)
            } else {
                "none yet".to_string()
            },
            (!link.has_session()).then_some(settings.warn),
        );
        reading(
            ui,
            "Would not decrypt",
            &count(link.decrypt_errors),
            (link.decrypt_errors > 0).then_some(settings.warn),
        );
        // The number that says how hard the link is working: loss the user
        // never saw, because the erasure code put it back.
        reading(
            ui,
            "Repaired by FEC",
            &format!(
                "{} ({:.2}%)",
                count(link.agg.recovered),
                link.recovery_pct()
            ),
            Some(loss_color(link.recovery_pct() as f32, settings)),
        );
        reading(
            ui,
            "Beyond repair",
            &format!("{} ({:.2}%)", count(link.agg.packets_lost), link.loss_pct()),
            Some(loss_color(link.loss_pct() as f32, settings)),
        );
        reading(ui, "Blocks overrun", &count(link.agg.overrun), None);
        reading(ui, "Queue drops", &count(radio.queue_drops), None);
        gap(ui, GAP_BLOCK);
    }

    #[cfg(not(feature = "radio"))]
    fn radio_readings(
        &self,
        _ui: &mut egui::Ui,
        _stats: &Stats,
        _settings: &crate::config::UiSettings,
    ) {
    }
}

/// Per-antenna signal, for telling a dead antenna from a weak link.
#[cfg(feature = "radio")]
fn antennas(signal: &crate::radio::Signal) -> String {
    let readings: Vec<String> = signal
        .antennas
        .iter()
        .filter_map(|dbm| dbm.map(|dbm| format!("{dbm}")))
        .collect();
    if readings.is_empty() {
        return "-".to_string();
    }
    format!("{} dBm", readings.join(" / "))
}

/// The color a signal strength is drawn in.
///
/// The thresholds are where an 802.11 link stops having margin rather than
/// where it stops working: -70 dBm still carries video, and is the point at
/// which a gust or a turn starts costing frames.
#[cfg(feature = "radio")]
fn signal_color(dbm: i32, settings: &crate::config::UiSettings) -> egui::Color32 {
    if dbm < -80 {
        settings.error
    } else if dbm < -70 {
        settings.warn
    } else {
        settings.ok
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
