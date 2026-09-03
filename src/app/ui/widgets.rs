//! The widget vocabulary the pages are written in.
//!
//! Every page is a list of declarations - this section, that reading, this
//! checkbox with that hover - and this module is where each of those is
//! spelled out once. A page file should read as *what is on the page*; the
//! `Area`/`Frame` scaffolding and the `RichText` incantations live here so
//! they do not have to.

use crate::app::SafeArea;

use super::theme::{em, gap, page_margin, GAP_HAIR, GAP_ITEM};

/// A full-screen page: a Background `Area` filled with the panel color, a
/// [`page_margin`] margin, sized to the screen, with both safe-area insets
/// already kept clear. The closure supplies the page's heading and body.
pub(super) fn content_page(
    ctx: &egui::Context,
    id: &str,
    screen: egui::Rect,
    safe: SafeArea,
    add: impl FnOnce(&mut egui::Ui),
) {
    // `Margin` counts in whole points, so the fractions are rounded once here
    // and the layout inside uses those same rounded values.
    let margin = page_margin(screen) as i8;
    // The bottom inset is part of the frame's margin rather than space added
    // after the content, and that is the point of it: a `ScrollArea` sizes its
    // viewport to the height it is given, so the inset has to come off that
    // height to keep the last row of a scrolled page above the gesture bar.
    let foot = margin.saturating_add(safe.bottom as i8);
    egui::Area::new(egui::Id::new(id))
        .order(egui::Order::Background)
        .fixed_pos(egui::Pos2::ZERO)
        .movable(false)
        .constrain(false)
        .show(ctx, |ui| {
            egui::Frame::NONE
                .fill(ui.visuals().panel_fill)
                .inner_margin(egui::Margin {
                    left: margin,
                    right: margin,
                    top: margin,
                    bottom: foot,
                })
                .show(ui, |ui| {
                    // An Area sizes itself to whatever it held last frame, so
                    // its Ui has no width to wrap against until something pins
                    // one: without this a long label lays out as one endless
                    // line and widens the page instead of wrapping.
                    let margin = f32::from(margin);
                    ui.set_width(screen.width() - 2.0 * margin);
                    ui.set_min_height(screen.height() - margin - f32::from(foot));
                    ui.add_space(safe.top);
                    gap(ui, GAP_ITEM);
                    add(ui);
                });
        });
}

/// A page heading.
pub(super) fn heading(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).heading().strong());
    gap(ui, GAP_ITEM);
}

/// A section heading with an optional line of explanation under it.
pub(super) fn section(ui: &mut egui::Ui, title: &str, hint: Option<&str>) {
    gap(ui, GAP_ITEM);
    ui.label(egui::RichText::new(title).strong());
    if let Some(hint) = hint {
        ui.label(egui::RichText::new(hint).weak().small());
    }
    gap(ui, GAP_HAIR);
}

/// One labelled reading: the name on the left, the value on the right in the
/// color the caller chose.
///
/// The value is monospaced so a number that changes every frame does not make
/// the row jitter as its digits change width - which at 60 fps is the
/// difference between a readable panel and a twitching one.
pub(super) fn reading(ui: &mut egui::Ui, label: &str, value: &str, color: Option<egui::Color32>) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).weak());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let mut text = egui::RichText::new(value).monospace();
            if let Some(color) = color {
                text = text.color(color);
            }
            ui.label(text);
        });
    });
}

/// A line of feedback under a control: green for done, red for failed.
pub(super) fn feedback(
    ui: &mut egui::Ui,
    message: &Result<String, String>,
    ok: egui::Color32,
    bad: egui::Color32,
) {
    let (text, color) = match message {
        Ok(text) => (text, ok),
        Err(text) => (text, bad),
    };
    ui.label(egui::RichText::new(text).color(color).small());
}

/// A full-width button, which on a phone is the only shape a primary action
/// can usefully be.
pub(super) fn wide_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let height = super::theme::control_height(ui);
    ui.add_sized(
        egui::vec2(ui.available_width(), height),
        egui::Button::new(text),
    )
}

/// The hamburger-and-X corner toggle, painted rather than drawn from an asset.
///
/// gps-gui-rs tints SVG icons for this; here there are only two glyphs and
/// they are three lines and two lines, so painting them keeps the crate free
/// of an SVG rasterizer and an assets directory for no loss.
pub(super) fn menu_glyph(ui: &egui::Ui, rect: egui::Rect, open: f32, color: egui::Color32) {
    let painter = ui.painter();
    let size = rect.width().min(rect.height());
    let arm = size * 0.28;
    let center = rect.center();
    let thickness = (size * 0.08).max(1.5);
    let spacing = size * 0.26;

    // The hamburger fades out as the X fades in, so the two are painted at
    // complementary alphas rather than switched between.
    let bars = color.gamma_multiply(1.0 - open);
    let cross = color.gamma_multiply(open);

    for row in -1..=1 {
        let y = center.y + row as f32 * spacing;
        painter.line_segment(
            [egui::pos2(center.x - arm, y), egui::pos2(center.x + arm, y)],
            egui::Stroke::new(thickness, bars),
        );
    }

    let stroke = egui::Stroke::new(thickness, cross);
    painter.line_segment(
        [
            egui::pos2(center.x - arm, center.y - arm),
            egui::pos2(center.x + arm, center.y + arm),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(center.x - arm, center.y + arm),
            egui::pos2(center.x + arm, center.y - arm),
        ],
        stroke,
    );
}

/// A floating panel over the video: a rounded, translucent frame that stays
/// readable over any picture.
pub(super) fn overlay_frame(ui: &egui::Ui) -> egui::Frame {
    let unit = em(ui);
    egui::Frame::NONE
        // Dark rather than the theme's panel color: this sits over the video,
        // where the only thing that reliably contrasts with an arbitrary
        // picture is something much darker than most of one.
        .fill(egui::Color32::from_black_alpha(160))
        .corner_radius(unit * 0.4)
        .inner_margin(egui::Margin::symmetric(
            (unit * 0.6) as i8,
            (unit * 0.4) as i8,
        ))
}
