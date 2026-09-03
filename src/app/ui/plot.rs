//! The link history graph: bitrate and loss over the last minute.
//!
//! Hand-painted rather than pulled from a plotting crate, for the same reason
//! the icons are: it draws two series with no axes, no legend and no
//! interaction, and that is a dozen lines of `Painter` calls against a
//! dependency with its own layout model to fight.
//!
//! The two series share an x axis and have separate y axes, which normally
//! makes a chart unreadable. It works here because they are not being compared
//! to each other - the question is "did they move at the same moment", and
//! that is exactly what a shared x axis answers.

use crate::app::Sample;

use super::theme::em;

/// Graph height as a fraction of the screen height, floored so it stays a
/// graph rather than a line on a short window.
const HEIGHT_FRAC: f32 = 0.18;
const HEIGHT_MIN_EM: f32 = 5.0;

/// Loss is drawn against a fixed ceiling rather than the data's own maximum.
///
/// An auto-scaled loss axis is actively misleading: a link losing 0.2% would
/// fill the graph exactly like one losing 40%, and the shape of a healthy link
/// would look alarming. Ten percent is where this link is genuinely in
/// trouble, so that is the top of the scale, and worse than that pins.
const LOSS_CEILING: f32 = 10.0;

/// Draw the history graph, or a placeholder line if there is nothing yet.
pub(super) fn link_history(
    ui: &mut egui::Ui,
    history: &[Sample],
    window_s: f64,
    bitrate_color: egui::Color32,
    loss_color: egui::Color32,
) {
    let unit = em(ui);
    let height = (ui.available_width() * HEIGHT_FRAC).max(unit * HEIGHT_MIN_EM);
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::hover(),
    );

    let painter = ui.painter();
    let visuals = ui.visuals();
    painter.rect_filled(rect, unit * 0.2, visuals.extreme_bg_color);

    if history.len() < 2 {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "collecting",
            egui::FontId::proportional(unit * 0.8),
            visuals.weak_text_color(),
        );
        return;
    }

    // The newest sample is "now" and the x axis runs back from it, so the
    // trace stays pinned to the right edge instead of sliding as the window
    // fills.
    let newest = history[history.len() - 1].at;
    let peak_bitrate = history.iter().map(|s| s.bitrate_bps).fold(1.0f64, f64::max);

    let x_for = |sample: &Sample| {
        let age = newest.duration_since(sample.at).as_secs_f64();
        let t = (1.0 - age / window_s).clamp(0.0, 1.0) as f32;
        rect.left() + rect.width() * t
    };
    let y_for = |fraction: f32| rect.bottom() - rect.height() * fraction.clamp(0.0, 1.0);

    // Bitrate against its own peak: the absolute value is on the readings
    // above, so what this axis is for is the shape.
    let bitrate: Vec<egui::Pos2> = history
        .iter()
        .map(|s| egui::pos2(x_for(s), y_for((s.bitrate_bps / peak_bitrate) as f32)))
        .collect();
    let loss: Vec<egui::Pos2> = history
        .iter()
        .map(|s| egui::pos2(x_for(s), y_for(s.loss_pct / LOSS_CEILING)))
        .collect();

    let width = (unit * 0.15).max(1.0);
    painter.add(egui::Shape::line(
        bitrate,
        egui::Stroke::new(width, bitrate_color),
    ));
    painter.add(egui::Shape::line(
        loss,
        egui::Stroke::new(width, loss_color),
    ));
}
