//! The menu page and the floating corner toggle that opens it.
//!
//! The menu is a page rather than a dropdown because it is the app's only
//! navigation: on a phone a list of full-width touch targets is what that has
//! to be, and a page is the one thing that always has room for one.

use crate::app::{DroneApp, Page, SafeArea};

use super::text;
use super::theme::{corner_margin, em, icon_size_for, GAP_ITEM};
use super::widgets::{content_page, heading, menu_glyph};

/// Every page in menu order, with its label. [`Page::Menu`] is deliberately
/// absent: it is the page doing the listing, so a button back to it would go
/// nowhere.
const PAGES: [(Page, &str); 3] = [
    (Page::Video, "Video"),
    (Page::Link, "Link"),
    (Page::Settings, "Settings"),
];

/// Menu-row height as a fraction of the corner-button size, which is itself a
/// fraction of the screen. Written against that rather than the body text
/// because these are touch targets first.
const ROW_H_FRAC: f32 = 1.3;

/// How long the hamburger takes to cross-fade into the X, in seconds.
const TOGGLE_FADE_S: f32 = 0.15;

impl DroneApp {
    /// The floating button that opens the menu, and closes it again.
    ///
    /// Drawn over every page, the menu page included, where it is what leaves
    /// without choosing anything. It sits inside the safe area, because on a
    /// phone the corner it wants is exactly where the status bar is.
    pub(in crate::app) fn corner_toggle(
        &mut self,
        ctx: &egui::Context,
        screen: egui::Rect,
        safe: SafeArea,
    ) {
        let size = icon_size_for(screen);
        let margin = corner_margin(screen);
        let open = self.page == Page::Menu;

        egui::Area::new(egui::Id::new("corner_toggle"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(
                screen.right() - size - margin - safe.right,
                screen.top() + margin + safe.top,
            ))
            .show(ctx, |ui| {
                let (rect, response) =
                    ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click());

                // A filled disc under the glyph: the toggle sits over video as
                // often as over a page, and three thin lines on an arbitrary
                // picture are invisible about half the time.
                ui.painter().circle_filled(
                    rect.center(),
                    size * 0.5,
                    egui::Color32::from_black_alpha(if open { 120 } else { 90 }),
                );

                let t = ui.ctx().animate_bool_with_time(
                    egui::Id::new("menu_glyph"),
                    open,
                    TOGGLE_FADE_S,
                );
                menu_glyph(ui, rect, t, egui::Color32::WHITE);

                if response.clicked() {
                    if open {
                        self.page = self.menu_from;
                    } else {
                        self.menu_from = self.page;
                        self.page = Page::Menu;
                    }
                }
                response.on_hover_text(if open {
                    text::MENU_CLOSE
                } else {
                    text::MENU_OPEN
                });
            });
    }

    /// The menu page itself: one full-width button per page.
    pub(in crate::app) fn page_menu_page(
        &mut self,
        ctx: &egui::Context,
        screen: egui::Rect,
        safe: SafeArea,
    ) {
        let row_height = icon_size_for(screen) * ROW_H_FRAC;

        content_page(ctx, "page_menu", screen, safe, |ui| {
            heading(ui, "drone-app");

            for (page, label) in PAGES {
                let selected = page == self.menu_from;
                let button =
                    egui::Button::new(egui::RichText::new(label).strong()).selected(selected);
                if ui
                    .add_sized(egui::vec2(ui.available_width(), row_height), button)
                    .clicked()
                {
                    self.page = page;
                }
                ui.add_space(em(ui) * GAP_ITEM);
            }
        });
    }
}
