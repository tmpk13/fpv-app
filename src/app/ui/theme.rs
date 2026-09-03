//! The measures every page is written in, and the functions that turn them
//! into points for the current screen.
//!
//! Nothing here draws. It is the one place a size is decided, so a page reads
//! as `gap(ui, GAP_SECTION)` rather than as a point count.
//!
//! Almost every measure is a fraction - of the screen for touch targets and
//! overlays, of the body text height for anything sitting next to text - so a
//! page keeps its proportions on a phone and on a desktop. A fixed point count
//! reads as cramped on one and loose on the other.

/// The smallest a control may be, in points: the floor under the height of
/// every button, field, checkbox and dropdown on a page.
///
/// The one measure in the UI that stays absolute. Everything else here is a
/// fraction of the screen or of the text, but this is a touch target, and a
/// fingertip is the same size whatever the screen is.
pub(super) const TOUCH_MIN: f32 = 40.0;

/// Corner-button side length as a fraction of the smaller screen dimension,
/// held between [`TOUCH_MIN`] and this ceiling.
const ICON_SIZE_FRAC: f32 = 0.05;
const ICON_SIZE_MAX: f32 = 64.0;

/// Inset of a floating corner control from the screen edge, as a fraction of
/// the smaller screen dimension.
const CORNER_MARGIN_FRAC: f32 = 0.03;

/// Margin between a content page's body and the screen edge, as a fraction of
/// the smaller screen dimension.
const PAGE_MARGIN_FRAC: f32 = 0.025;

/// The vertical rhythm of the pages, in body-text heights.
pub(super) const GAP_HAIR: f32 = 0.25;
pub(super) const GAP_ITEM: f32 = 0.5;
pub(super) const GAP_BLOCK: f32 = 0.75;
pub(super) const GAP_SECTION: f32 = 1.0;

/// A text input is a fraction of the screen width, held between these two
/// widths in text units: wide enough to type in on a phone, and not sprawling
/// across a desktop window.
const FIELD_MIN_EM: f32 = 6.0;
const FIELD_MAX_EM: f32 = 18.0;

/// Square corner-button side length in points for the current screen size.
pub(super) fn icon_size_for(screen: egui::Rect) -> f32 {
    (screen.size().min_elem() * ICON_SIZE_FRAC).clamp(TOUCH_MIN, ICON_SIZE_MAX)
}

/// The body text height: the unit the page measures are written in.
pub(super) fn em(ui: &egui::Ui) -> f32 {
    ui.text_style_height(&egui::TextStyle::Body)
}

/// Vertical space of `ems` body-text heights.
pub(super) fn gap(ui: &mut egui::Ui, ems: f32) {
    let space = em(ui) * ems;
    ui.add_space(space);
}

/// Margin between a page's body and the screen edge, in points.
pub(super) fn page_margin(screen: egui::Rect) -> f32 {
    screen.size().min_elem() * PAGE_MARGIN_FRAC
}

/// Inset of a floating corner control from the screen edge, in points.
pub(super) fn corner_margin(screen: egui::Rect) -> f32 {
    screen.size().min_elem() * CORNER_MARGIN_FRAC
}

/// Width for a text input: `frac` of the screen width, kept readable.
pub(super) fn field_width(ui: &egui::Ui, screen: egui::Rect, frac: f32) -> f32 {
    let em = em(ui);
    (screen.width() * frac).clamp(em * FIELD_MIN_EM, em * FIELD_MAX_EM)
}

/// Measures for egui's own `Spacing`, in body-font sizes. These are the
/// insides of a control - what [`gap`] and [`field_width`] are to the space
/// between them.
///
/// Every one is a multiple of the body font, so they hold their proportions at
/// any `text_scale`; only [`TOUCH_MIN`] is absolute.
const BUTTON_PAD_X_EM: f32 = 0.6;
const BUTTON_PAD_Y_EM: f32 = 0.3;
const ITEM_SPACING_X_EM: f32 = 0.55;
const ITEM_SPACING_Y_EM: f32 = 0.35;
const CONTROL_HEIGHT_EM: f32 = 2.6;
const CONTROL_WIDTH_EM: f32 = 3.2;
const CHECK_EM: f32 = 1.2;
const CHECK_INNER_EM: f32 = 0.7;
const INDENT_EM: f32 = 1.4;
const COMBO_EM: f32 = 8.0;

/// The scroll bar, in body-font sizes, with a floor so it stays wide enough to
/// catch a finger.
const SCROLL_BAR_EM: f32 = 0.55;
const SCROLL_BAR_MIN: f32 = 8.0;

/// egui's own body font size, as a fallback only: the style always has a
/// `Body` entry by the time this runs.
const DEFAULT_BODY_PT: f32 = 12.5;

/// Size every interactive control off the body text, with a touch-target floor
/// under the lot.
///
/// egui's stock spacing is a set of absolute point counts, which leaves two
/// problems that this fixes together:
///
/// - A button is 18 points tall before padding, which is under half a
///   fingertip.
/// - Scaling the text scales the *font*; the padding, the checkbox glyph and
///   the minimum control height are not fonts, so without this they stay put
///   and large text ends up in cramped rows.
pub(in crate::app) fn apply_spacing(style: &mut egui::Style) {
    let font = style
        .text_styles
        .get(&egui::TextStyle::Body)
        .map_or(DEFAULT_BODY_PT, |id| id.size);
    let spacing = &mut style.spacing;
    spacing.button_padding = egui::vec2(font * BUTTON_PAD_X_EM, font * BUTTON_PAD_Y_EM);
    spacing.item_spacing = egui::vec2(font * ITEM_SPACING_X_EM, font * ITEM_SPACING_Y_EM);
    // `interact_size.y` is the floor egui puts under a button, a checkbox, a
    // radio, a drag value, a slider and a dropdown, so this one line is most
    // of what makes them all tappable.
    spacing.interact_size.y = (font * CONTROL_HEIGHT_EM).max(TOUCH_MIN);
    spacing.interact_size.x = (font * CONTROL_WIDTH_EM).max(TOUCH_MIN);
    spacing.icon_width = font * CHECK_EM;
    spacing.icon_width_inner = font * CHECK_INNER_EM;
    spacing.icon_spacing = font * ITEM_SPACING_X_EM;
    spacing.indent = font * INDENT_EM;
    spacing.combo_width = font * COMBO_EM;
    spacing.scroll.bar_width = (font * SCROLL_BAR_EM).max(SCROLL_BAR_MIN);
}

/// The height every interactive control is laid out at, in points.
pub(super) fn control_height(ui: &egui::Ui) -> f32 {
    ui.spacing().interact_size.y
}
