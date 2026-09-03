//! The Settings page: where the stream arrives and how it is drawn.
//!
//! Every control here edits [`crate::app::Draft`] rather than the live config,
//! and nothing takes effect until Apply. That is deliberate for the port and
//! the bind address: applied per keystroke, typing "5601" over "5600" would
//! rebind the socket four times on the way, twice to ports nothing is sending
//! to. The picture settings could apply live, but a page where some controls
//! commit and others do not is worse than one where none do.

use crate::app::{DroneApp, SafeArea};
use crate::video::Codec;

use super::super::text;
use super::super::theme::{field_width, gap, GAP_BLOCK, GAP_SECTION};
use super::super::widgets::{content_page, feedback, heading, section, wide_button};

/// Text-field width as a fraction of the screen.
const FIELD_FRAC: f32 = 0.35;

/// The range the text scale can be dragged over. The floor is what keeps a
/// slip from making the page unreadable and therefore unfixable.
const TEXT_SCALE_RANGE: std::ops::RangeInclusive<f32> = 0.7..=2.0;

impl DroneApp {
    pub(in crate::app) fn page_settings(
        &mut self,
        ctx: &egui::Context,
        screen: egui::Rect,
        safe: SafeArea,
    ) {
        let settings = self.ui_settings();

        content_page(ctx, "page_settings", screen, safe, |ui| {
            heading(ui, "Settings");
            // Measured here rather than before the page: the width is a
            // fraction of the screen held to a range of text widths, and there
            // is no text height to hold it against until a Ui exists.
            let field = field_width(ui, screen, FIELD_FRAC);

            egui::ScrollArea::vertical().show(ui, |ui| {
                section(ui, "Source", Some(text::SETTINGS_SOURCE_HINT));

                ui.horizontal(|ui| {
                    ui.label("Listen on");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.draft.bind)
                            .desired_width(field)
                            .hint_text("0.0.0.0"),
                    )
                    .on_hover_text(text::SETTINGS_BIND_HINT);
                });
                ui.horizontal(|ui| {
                    ui.label("Port");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.draft.port)
                            .desired_width(field * 0.5)
                            .hint_text("5600"),
                    );
                });

                gap(ui, GAP_BLOCK);
                ui.horizontal(|ui| {
                    ui.label("Codec");
                    egui::ComboBox::from_id_salt("codec")
                        .selected_text(codec_label(self.draft.codec))
                        .show_ui(ui, |ui| {
                            for option in [None, Some(Codec::H264), Some(Codec::H265)] {
                                ui.selectable_value(
                                    &mut self.draft.codec,
                                    option,
                                    codec_label(option),
                                );
                            }
                        })
                        .response
                        .on_hover_text(text::SETTINGS_CODEC_HINT);
                });

                section(ui, "Picture", Some(text::SETTINGS_VIDEO_HINT));
                ui.checkbox(&mut self.draft.fill, "Fill the screen (crops the edges)");
                ui.checkbox(&mut self.draft.overlay, "Show the readout over the picture");
                ui.checkbox(&mut self.draft.smooth, "Smooth scaling");

                section(ui, "Text", Some(text::SETTINGS_UI_HINT));
                ui.add(
                    egui::Slider::new(&mut self.draft.text_scale, TEXT_SCALE_RANGE)
                        .text("Size")
                        .fixed_decimals(2),
                );

                gap(ui, GAP_SECTION);
                if wide_button(ui, "Apply and save")
                    .on_hover_text(text::SETTINGS_SAVE)
                    .clicked()
                {
                    self.apply_draft();
                }

                if let Some(result) = self.save_result.as_ref() {
                    gap(ui, GAP_BLOCK);
                    feedback(ui, result, settings.ok, settings.error);
                }

                if let Some(path) = self.config.path.as_ref() {
                    gap(ui, GAP_BLOCK);
                    ui.label(
                        egui::RichText::new(format!("Config file: {}", path.display()))
                            .weak()
                            .small(),
                    );
                }
            });
        });
    }
}

/// The label for a codec setting in the dropdown.
fn codec_label(codec: Option<Codec>) -> &'static str {
    match codec {
        None => "Auto",
        Some(Codec::H264) => "H.264",
        Some(Codec::H265) => "H.265",
    }
}
