//! The view layer: what each page looks like, kept apart from what the app
//! does.
//!
//! [`crate::app`] owns the state and the per-frame loop; everything here
//! draws. The split inside this module is by *kind of thing*, so a change has
//! one obvious home:
//!
//! - [`pages`] - one file per page, each a list of declarations: this section,
//!   that reading, bound to this field.
//! - [`widgets`] - the vocabulary those declarations are written in.
//! - [`theme`] - every size and spacing in the app, as a named measure.
//! - [`text`] - the long-form prose and hover texts.
//! - [`menu`] - the menu page and the corner toggle that opens it.
//! - [`plot`] - the link history graph, kept out of the page that frames it.

mod menu;
mod pages;
mod plot;
mod text;
mod theme;
mod widgets;

/// Size every control off the body text, with a touch-target floor under the
/// lot. Applied to the style by [`crate::app::DroneApp::apply_ui_style`],
/// beside the text sizes it is derived from.
pub(super) use theme::apply_spacing;

use crate::app::DroneApp;
use crate::config::UiSettings;

/// The inputs the applied style depends on.
///
/// Kept so the style is only rewritten when one of them changes. Rewriting it
/// every frame is not free - it clones and replaces every `Style` in the
/// context - and this app runs at frame rate for as long as video is arriving.
#[derive(Clone, Copy, PartialEq)]
pub(super) struct AppliedStyle {
    theme: egui::Theme,
    background: Option<egui::Color32>,
    text: Option<egui::Color32>,
    text_scale: f32,
}

impl AppliedStyle {
    fn of(settings: &UiSettings, theme: egui::Theme) -> Self {
        Self {
            theme,
            background: settings.background,
            text: settings.text,
            text_scale: settings.text_scale,
        }
    }
}

impl DroneApp {
    /// Apply the configured colors and text scale to the whole style.
    ///
    /// Applied to the style rather than per page so it reaches the dropdown
    /// popups, the sliders and the scroll bars as well as the pages.
    pub(super) fn apply_ui_style(&mut self, ctx: &egui::Context) {
        let settings = self.ui_settings();
        let theme = ctx.theme();
        let wanted = AppliedStyle::of(&settings, theme);
        if self.style_applied == Some(wanted) {
            return;
        }
        self.style_applied = Some(wanted);

        // Text size first, and for both themes: the sizes are the same either
        // way, so switching theme has nothing to redo here. Scaling egui's own
        // defaults preserves the relationship between heading, body and small
        // instead of inventing a second type scale.
        let scale = settings.text_scale;
        ctx.all_styles_mut(|style| {
            style.text_styles = egui::style::default_text_styles()
                .into_iter()
                .map(|(name, font)| (name, egui::FontId::new(font.size * scale, font.family)))
                .collect();
            // The controls are measured off the text, so they are rewritten
            // here with it rather than being left on egui's absolute defaults.
            apply_spacing(style);
        });

        let mut visuals = theme.default_visuals();
        if let Some(color) = settings.background {
            visuals.panel_fill = color;
            visuals.window_fill = color;
        }
        if let Some(color) = settings.text {
            // Every widget state's foreground, which is where egui reads text
            // and the checkmarks from - rather than `override_text_color`,
            // which reaches the plain labels only and would leave the rest on
            // the theme.
            for widget in [
                &mut visuals.widgets.noninteractive,
                &mut visuals.widgets.inactive,
                &mut visuals.widgets.hovered,
                &mut visuals.widgets.open,
            ] {
                widget.fg_stroke.color = color;
            }
            // `strong` text (the section headings) reads the active state,
            // which the theme keeps a step past the body text. Shading the
            // configured color the same way keeps the emphasis without a
            // second setting for it.
            let past = if visuals.dark_mode {
                egui::Color32::WHITE
            } else {
                egui::Color32::BLACK
            };
            visuals.widgets.active.fg_stroke.color = color.lerp_to_gamma(past, 0.35);
        }
        ctx.set_visuals_of(theme, visuals);
    }
}
