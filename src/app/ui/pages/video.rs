//! The video page: the picture, and what to say when there is not one.
//!
//! The picture is drawn as a full-screen background rather than inside a
//! panel, so nothing on this page has a margin: an FPV feed with a border
//! around it wastes the one thing the screen is for. Everything else here
//! floats over it.

use crate::app::{DroneApp, SafeArea};
use crate::video::Stats;

use super::super::text;
use super::super::theme::{corner_margin, em, GAP_HAIR};
use super::super::widgets::overlay_frame;

/// Width of the "no video" card as a fraction of the screen, so the prose has
/// a sane measure on a phone and does not sprawl on a desktop.
const CARD_WIDTH_FRAC: f32 = 0.8;
const CARD_WIDTH_MAX_EM: f32 = 34.0;

impl DroneApp {
    pub(in crate::app) fn page_video(
        &mut self,
        ctx: &egui::Context,
        screen: egui::Rect,
        safe: SafeArea,
        stats: &Stats,
    ) {
        // The whole screen, under everything else.
        egui::Area::new(egui::Id::new("page_video"))
            .order(egui::Order::Background)
            .fixed_pos(egui::Pos2::ZERO)
            .movable(false)
            .constrain(false)
            .show(ctx, |ui| {
                let (rect, response) = ui.allocate_exact_size(screen.size(), egui::Sense::click());
                let clicked = response.clicked();
                // Black rather than the panel color: it is the letterbox
                // around the picture, and grey bars read as a broken layout
                // where black reads as the edge of the frame.
                ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);

                match self.video_texture() {
                    Some((texture, size)) => {
                        let (target, uv) = picture_placement(rect, size, self.config.video.fill);
                        ui.painter()
                            .image(texture.id(), target, uv, egui::Color32::WHITE);
                    }
                    None => self.draw_no_video(ui, rect, stats),
                }

                // Tapping the picture switches fit and fill. A tap is the only
                // gesture a phone reliably has here, and this is the only
                // setting worth reaching without opening a page. The draft is
                // moved with it so the Settings page does not then show the
                // old value and undo this on its next Apply.
                if self.video_texture().is_some() {
                    response.on_hover_text(text::VIDEO_HINT);
                    if clicked {
                        self.config.video.fill = !self.config.video.fill;
                        self.draft.fill = self.config.video.fill;
                    }
                }
            });

        if self.config.video.overlay {
            self.draw_overlay(ctx, screen, safe, stats);
        }
    }

    /// The current picture and its size, if one has been decoded.
    fn video_texture(&self) -> Option<(&egui::TextureHandle, [usize; 2])> {
        let texture = self.texture.as_ref()?;
        let size = texture.size();
        if size[0] == 0 || size[1] == 0 {
            return None;
        }
        Some((texture, size))
    }

    /// The floating readout in the top-left corner.
    fn draw_overlay(&self, ctx: &egui::Context, screen: egui::Rect, safe: SafeArea, stats: &Stats) {
        let margin = corner_margin(screen);
        let ui_settings = self.ui_settings();

        egui::Area::new(egui::Id::new("video_overlay"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(
                screen.left() + margin + safe.left,
                screen.top() + margin + safe.top,
            ))
            .show(ctx, |ui| {
                overlay_frame(ui).show(ui, |ui| {
                    let color = health_color(stats, &ui_settings);
                    let codec = stats
                        .codec
                        .map_or_else(|| "no signal".to_string(), |c| c.to_string());

                    let line = if stats.width > 0 {
                        format!(
                            "{codec}  {}x{}  {:.0} fps",
                            stats.width, stats.height, stats.fps
                        )
                    } else {
                        codec
                    };
                    ui.label(
                        egui::RichText::new(line)
                            .monospace()
                            .color(egui::Color32::WHITE),
                    );
                    ui.add_space(em(ui) * GAP_HAIR);
                    ui.label(
                        egui::RichText::new(format!(
                            "{}  {:.1}% loss",
                            super::bitrate(stats.bitrate_bps),
                            stats.rtp.loss_pct()
                        ))
                        .monospace()
                        .color(color),
                    );
                });
            });
    }

    /// The card that explains why the screen is black.
    fn draw_no_video(&self, ui: &mut egui::Ui, rect: egui::Rect, stats: &Stats) {
        let unit = em(ui);
        let width = (rect.width() * CARD_WIDTH_FRAC).min(unit * CARD_WIDTH_MAX_EM);
        let bind = self.config.source.bind.to_string();
        let port = self.config.source.port;

        // Each of these is a different fault with a different fix. Reporting
        // them apart is most of this page's value when the link is down, which
        // is exactly when a bare black screen is least helpful.
        let body = if let Some(reason) = stats.bind_error {
            text::bind_failed(reason, &bind, port)
        } else {
            match (stats.since_packet_s, stats.since_frame_s) {
                (None, _) => text::no_packets(&bind, port),
                (Some(quiet), _) if quiet > 2.0 => text::packets_stopped(quiet),
                (Some(_), None) => text::NO_FRAMES.to_string(),
                (Some(_), Some(_)) => text::NO_FRAMES.to_string(),
            }
        };

        let center = rect.center();
        let area = egui::Rect::from_center_size(center, egui::vec2(width, rect.height()));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area));
        child.vertical_centered(|ui| {
            ui.set_width(width);
            ui.add_space(rect.height() * 0.3);
            ui.label(
                egui::RichText::new(text::NO_VIDEO_TITLE)
                    .heading()
                    .color(egui::Color32::WHITE),
            );
            ui.add_space(unit);
            ui.label(
                egui::RichText::new(body)
                    .color(egui::Color32::from_gray(190))
                    .monospace()
                    .small(),
            );
        });
    }
}

/// Where a picture of `size` goes inside `rect`, as `(target, uv)`.
///
/// `target` is the rectangle on screen to paint into and `uv` is the part of
/// the texture to take, in 0..1. Both preserve the aspect ratio:
///
/// - Fitting letterboxes - the whole frame is visible, with black at two
///   edges. Right when everything in the picture matters.
/// - Filling crops - the screen is covered and the overflow is taken off the
///   texture coordinates rather than painted outside the window. Right on a
///   phone held in the other orientation from the camera, where letterboxing
///   leaves a picture the size of a stamp.
///
/// Pure, and separate from the painting, because it is the one piece of this
/// page with arithmetic worth checking: a fit that crops or a fill that
/// letterboxes both look plausible until measured.
fn picture_placement(rect: egui::Rect, size: [usize; 2], fill: bool) -> (egui::Rect, egui::Rect) {
    let full_uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
    let picture = egui::vec2(size[0] as f32, size[1] as f32);
    if picture.x <= 0.0 || picture.y <= 0.0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return (rect, full_uv);
    }

    let scale_x = rect.width() / picture.x;
    let scale_y = rect.height() / picture.y;

    if fill {
        // The larger scale leaves nothing blank; what falls outside the
        // rectangle is removed from the source instead of being painted.
        let scale = scale_x.max(scale_y);
        let drawn = picture * scale;
        let visible = egui::vec2(
            (rect.width() / drawn.x).min(1.0),
            (rect.height() / drawn.y).min(1.0),
        );
        let uv = egui::Rect::from_center_size(egui::pos2(0.5, 0.5), visible);
        (rect, uv)
    } else {
        // The smaller scale keeps the whole picture, centered in the space.
        let scale = scale_x.min(scale_y);
        let target = egui::Rect::from_center_size(rect.center(), picture * scale);
        (target, full_uv)
    }
}

/// The color a link readout is drawn in: fine, degraded, or gone.
fn health_color(stats: &Stats, settings: &crate::config::UiSettings) -> egui::Color32 {
    if !stats.live() {
        return settings.error;
    }
    // Under a percent is ordinary on this link and not worth flagging; over
    // five is the margin visibly going.
    if stats.rtp.loss_pct() > 5.0 {
        settings.warn
    } else {
        settings.ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 16:9 screen and a 16:9 picture, and a 4:3 picture for the mismatched
    /// cases.
    fn screen() -> egui::Rect {
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 900.0))
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn fitting_a_matching_aspect_ratio_fills_exactly() {
        let (target, uv) = picture_placement(screen(), [1920, 1080], false);
        assert!(close(target.width(), 1600.0));
        assert!(close(target.height(), 900.0));
        assert_eq!(
            uv,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0))
        );
    }

    #[test]
    fn fitting_a_taller_picture_letterboxes_at_the_sides() {
        // 4:3 into 16:9: height is the limit, so the picture is narrower than
        // the screen and nothing is cropped.
        let (target, uv) = picture_placement(screen(), [640, 480], false);
        assert!(close(target.height(), 900.0), "height should be the limit");
        assert!(
            close(target.width(), 1200.0),
            "4:3 at 900 tall is 1200 wide"
        );
        assert!(target.width() < screen().width());
        assert_eq!(
            uv,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
            "fitting must never crop the source"
        );
        // Centered, so the two black bars are equal.
        assert!(close(target.center().x, screen().center().x));
    }

    #[test]
    fn fitting_a_wider_picture_letterboxes_above_and_below() {
        // 2.39:1 into 16:9: width is the limit.
        let (target, _) = picture_placement(screen(), [2390, 1000], false);
        assert!(close(target.width(), 1600.0));
        assert!(target.height() < screen().height());
        assert!(close(target.center().y, screen().center().y));
    }

    #[test]
    fn filling_covers_the_whole_screen() {
        for size in [[640, 480], [2390, 1000], [1920, 1080]] {
            let (target, _) = picture_placement(screen(), size, true);
            assert_eq!(target, screen(), "filling must leave no gap for {size:?}");
        }
    }

    #[test]
    fn filling_a_taller_picture_crops_its_top_and_bottom() {
        // 4:3 scaled to cover 16:9 overflows vertically, so the source is
        // narrowed in v and left whole in u.
        let (_, uv) = picture_placement(screen(), [640, 480], true);
        assert!(close(uv.width(), 1.0), "nothing should be cropped sideways");
        assert!(uv.height() < 1.0, "the top and bottom should be cropped");
        // Centered on the middle of the picture.
        assert!(close(uv.center().y, 0.5));
    }

    #[test]
    fn filling_a_wider_picture_crops_its_sides() {
        let (_, uv) = picture_placement(screen(), [2390, 1000], true);
        assert!(uv.width() < 1.0, "the sides should be cropped");
        assert!(close(uv.height(), 1.0));
        assert!(close(uv.center().x, 0.5));
    }

    #[test]
    fn filling_a_matching_aspect_ratio_crops_nothing() {
        let (target, uv) = picture_placement(screen(), [1920, 1080], true);
        assert_eq!(target, screen());
        assert!(close(uv.width(), 1.0));
        assert!(close(uv.height(), 1.0));
    }

    #[test]
    fn the_aspect_ratio_is_preserved_either_way() {
        let source = 640.0 / 480.0;
        // Fitting: the target's own ratio is the picture's.
        let (target, _) = picture_placement(screen(), [640, 480], false);
        assert!(close(target.width() / target.height(), source));

        // Filling: the target is the screen, so the ratio shows up in the
        // visible part of the source instead.
        let (target, uv) = picture_placement(screen(), [640, 480], true);
        let visible = egui::vec2(uv.width() * 640.0, uv.height() * 480.0);
        assert!(close(
            (target.width() / target.height()) / (visible.x / visible.y),
            1.0
        ));
    }

    #[test]
    fn a_degenerate_size_does_not_divide_by_zero() {
        for size in [[0, 1080], [1920, 0], [0, 0]] {
            let (target, uv) = picture_placement(screen(), size, false);
            assert_eq!(target, screen());
            assert!(uv.width().is_finite() && uv.height().is_finite());
        }
        let empty = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::ZERO);
        let (_, uv) = picture_placement(empty, [1920, 1080], true);
        assert!(uv.width().is_finite());
    }
}
