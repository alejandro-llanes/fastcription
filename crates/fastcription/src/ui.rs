//! The app's visual language: the handful of shapes every pane is built from.
//!
//! These used to be spelled out at each call site, so "a heading" was
//! `ui.heading` in one pane, a bold `RichText` in another and a plain label in
//! a third, and a button that mattered looked exactly like one that did not.
//! The interface read as a pile of widgets rather than as a designed thing.
//!
//! ## The grammar
//!
//! Three depths, and nothing else: the **window** is the ground, a **bar** is
//! a surface laid on it, and a **panel** is a card raised off that. Depth is
//! what tells a reader which things belong together, so it is spent on
//! grouping and never on decoration.
//!
//! Labels are small, upper-case and quiet; values are large and bright. The
//! pairing is lifted from audio hardware, where it exists because an operator
//! has to read a value across a room without hunting for which number goes
//! with which knob — which is close enough to this app's problem to borrow
//! from.
//!
//! ## Why the glow is real
//!
//! Accented controls carry a halo. egui has no blur, so a halo is a larger,
//! fainter copy of the shape drawn behind it; two of them read as light at a
//! glance. It is spent only on the thing that is currently true — the record
//! button while recording, the selected tab — so it means "this one" rather
//! than "this app has a style".
//!
//! Every colour comes from [`crate::theme::Palette`], which is whatever the
//! desktop is wearing. Nothing here invents one.

use egui::{Color32, CornerRadius, Margin, Rect, Response, RichText, Shape, Stroke, Ui, Vec2};

use crate::theme::Palette;

/// Corner radius for the two sizes of rounded thing: cards and controls.
pub const PANEL_RADIUS: u8 = 12;
pub const CONTROL_RADIUS: u8 = 8;

/// The height every pill-shaped control shares.
///
/// One number because a row of controls that disagree about their height
/// reads as a row of unrelated widgets, which is most of what was wrong with
/// the top bar.
pub const CONTROL_HEIGHT: f32 = 30.0;

/// Point size of the small upper-case labels.
pub const LABEL_PT: f32 = 10.0;

/// How much of the accent's brightness a halo keeps, outermost first.
const HALO: [(f32, f32); 2] = [(6.0, 0.10), (3.0, 0.18)];

/// A quiet upper-case label, for naming a group of controls or a readout.
///
/// Upper-case and letter-spaced rather than bold: a bold label competes with
/// the value under it for the eye, and the value is the part worth reading.
pub fn label(ui: &mut Ui, palette: &Palette, text: &str) -> Response {
    ui.label(
        RichText::new(spaced_caps(text))
            .size(LABEL_PT)
            .color(palette.dim),
    )
}

/// `TEXT LIKE THIS`, with a hair of space between the letters.
///
/// egui has no letter-spacing, and upper-case text without it sets too tight
/// to read at this size. A space between every character is too much; a thin
/// space is right and is one character.
fn spaced_caps(text: &str) -> String {
    let upper = text.to_uppercase();
    let mut out = String::with_capacity(upper.len() * 2);
    let mut previous = None;
    for ch in upper.chars() {
        // Between two letters only. A thin space after a real one would set
        // the word gap twice as wide as it should be.
        if previous.is_some_and(|p: char| p != ' ') && ch != ' ' {
            out.push('\u{2009}');
        }
        out.push(ch);
        previous = Some(ch);
    }
    out
}

/// A label with its value under it, the way a meter is labelled.
pub fn readout(ui: &mut Ui, palette: &Palette, name: &str, value: &str, strong: bool) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 1.0;
        label(ui, palette, name);
        ui.label(
            RichText::new(value)
                .size(14.0)
                .monospace()
                .color(if strong {
                    palette.text
                } else {
                    palette.secondary
                }),
        );
    });
}

/// Makes inset controls legible on a raised panel.
///
/// egui draws a slider's rail, a text field and a combo box in
/// `widgets.inactive.bg_fill`, which is the same `surface` a card is filled
/// with — so on a card the rail disappears and the handle is left looking like
/// a stray box someone forgot to delete. Dropping those to the window colour
/// puts the groove back, and gives every inset control the same relationship
/// to the card it sits on that the card has to the window.
pub fn inset_controls(ui: &mut Ui, palette: &Palette) {
    let visuals = ui.visuals_mut();
    visuals.widgets.inactive.bg_fill = palette.window;
    visuals.widgets.inactive.weak_bg_fill = palette.window;
    visuals.widgets.hovered.bg_fill = palette.surface_hover;
    visuals.widgets.hovered.weak_bg_fill = palette.surface_hover;
    visuals.extreme_bg_color = palette.window;
}

/// The raised panel a whole pane is read out of, filling the room it is given.
///
/// The transcript is the product, so it gets the one card in the window and
/// all of the space left over: what a reader is trying to follow should be the
/// largest and brightest thing in front of them, not one more widget among the
/// controls. The history pane uses it too — a conversation from the library
/// and one happening now are the same thing to a reader, and they used to be
/// framed differently.
pub fn pane<R>(ui: &mut Ui, palette: &Palette, body: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::default()
        .fill(palette.surface)
        .stroke(Stroke::new(1.0, palette.outline))
        .corner_radius(PANEL_RADIUS)
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            inset_controls(ui, palette);
            body(ui)
        })
        .inner
}

/// The frame a bar across the window gets: a surface, and a hairline where it
/// meets the ground.
///
/// `top` says which edge the hairline goes on, since the same shape serves the
/// header and the footer.
pub fn bar(palette: &Palette, top: bool) -> egui::Frame {
    egui::Frame::default()
        .fill(palette.panel)
        .inner_margin(Margin::symmetric(12, 8))
        .outer_margin(Margin::ZERO)
        // The hairline where the bar meets the ground, as a hard one-pixel
        // shadow: a `stroke` would outline all four sides, and a bar spanning
        // the window has no left or right edge to draw.
        .shadow(egui::epaint::Shadow {
            offset: [0, if top { 1 } else { -1 }],
            blur: 0,
            spread: 0,
            color: palette.outline,
        })
}

/// Reserves a slot in the paint list for a halo.
///
/// A halo goes *behind* its control, and egui paints in call order, so the
/// slot has to be taken before the control is added and filled in once the
/// control's rect is known. `Shape::Noop` is egui's placeholder for exactly
/// this.
fn reserve_halo(ui: &Ui) -> egui::layers::ShapeIdx {
    ui.painter().add(Shape::Noop)
}

/// Fills a slot reserved by [`reserve_halo`] with a halo around `rect`.
fn paint_halo(
    ui: &Ui,
    slot: egui::layers::ShapeIdx,
    rect: Rect,
    color: Color32,
    radius: impl Into<CornerRadius> + Copy,
) {
    let shapes: Vec<Shape> = HALO
        .iter()
        .map(|&(grow, alpha)| {
            Shape::rect_filled(
                rect.expand(grow),
                radius.into(),
                color.gamma_multiply(alpha),
            )
        })
        .collect();
    ui.painter().set(slot, Shape::Vec(shapes));
}

/// How a [`pill`] presents itself.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// The one action that matters here. Accent fill, and a halo.
    Primary,
    /// An ordinary control: raised, but not shouting.
    Normal,
    /// Part of a group where another member is chosen. No fill at all, so the
    /// chosen one is the only filled thing in the row.
    Quiet,
}

/// A rounded control with an optional icon and a label.
///
/// One function for every button in the chrome, because the previous
/// arrangement — `small_button` here, `selectable_label` there,
/// `Button::image_and_text` somewhere else — is exactly how a row ends up with
/// four different heights and three different corner radii in it.
pub fn pill(
    ui: &mut Ui,
    palette: &Palette,
    tone: Tone,
    icon: Option<egui::Image<'static>>,
    text: &str,
) -> Response {
    let (fill, fg) = match tone {
        Tone::Primary => (palette.accent, palette.on_accent),
        Tone::Normal => (palette.surface, palette.text),
        Tone::Quiet => (Color32::TRANSPARENT, palette.secondary),
    };
    let slot = reserve_halo(ui);
    let label = RichText::new(text).color(fg);
    let button = match icon {
        Some(image) => egui::Button::image_and_text(image, label),
        None => egui::Button::new(label),
    }
    .fill(fill)
    .stroke(match tone {
        Tone::Quiet => Stroke::NONE,
        _ => Stroke::new(1.0, palette.outline),
    })
    .corner_radius(CONTROL_RADIUS)
    .min_size(Vec2::new(0.0, CONTROL_HEIGHT));

    let response = ui.add(button);
    if tone == Tone::Primary {
        paint_halo(ui, slot, response.rect, palette.accent, CONTROL_RADIUS);
    }
    response
}

/// A row of mutually exclusive choices, the chosen one filled.
///
/// Returns the index clicked, if any. The caller keeps the state: this is a
/// view of a choice, not an owner of one.
pub fn segmented(ui: &mut Ui, palette: &Palette, options: &[&str], chosen: usize) -> Option<usize> {
    let mut clicked = None;
    egui::Frame::default()
        .fill(palette.window)
        .corner_radius(CONTROL_RADIUS + 2)
        .inner_margin(Margin::same(3))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            // The options are reversed rather than the layout being forced,
            // because this is usually placed in a bar that aligns its contents
            // to the right. Forcing a left-to-right child inside one of those
            // makes the child claim the whole remaining width, which threw the
            // control to the far side of the window; reversing the order
            // leaves it the size of its contents and still reading
            // left-to-right on screen.
            let backwards = ui.layout().prefer_right_to_left();
            let order: Vec<usize> = if backwards {
                (0..options.len()).rev().collect()
            } else {
                (0..options.len()).collect()
            };
            ui.horizontal(|ui| {
                for index in order {
                    let tone = if index == chosen {
                        Tone::Primary
                    } else {
                        Tone::Quiet
                    };
                    if pill(ui, palette, tone, None, options[index]).clicked() {
                        clicked = Some(index);
                    }
                }
            });
        });
    clicked
}

/// A round icon button, for the places a word will not fit.
pub fn icon_button(
    ui: &mut Ui,
    palette: &Palette,
    icon: crate::icons::Icon,
    active: bool,
) -> Response {
    let (fill, tint) = if active {
        (palette.accent, palette.on_accent)
    } else {
        (palette.surface, palette.text)
    };
    let slot = reserve_halo(ui);
    let response = ui.add(
        egui::Button::image(icon.image(tint, 14.0))
            .fill(fill)
            .stroke(Stroke::new(1.0, palette.outline))
            .corner_radius((CONTROL_HEIGHT / 2.0) as u8)
            .min_size(Vec2::splat(CONTROL_HEIGHT)),
    );
    if active {
        paint_halo(
            ui,
            slot,
            response.rect,
            palette.accent,
            (CONTROL_HEIGHT / 2.0) as u8,
        );
    }
    response
}

/// A small coloured capsule carrying one word of state.
pub fn badge(ui: &mut Ui, text: &str, color: Color32) -> Response {
    egui::Frame::default()
        .fill(color.gamma_multiply(0.18))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.45)))
        .corner_radius(CONTROL_RADIUS)
        .inner_margin(Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(11.0).color(color));
        })
        .response
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The labels are built by hand because egui has no letter-spacing, so the
    /// construction is worth pinning: upper-cased, a thin space between
    /// letters, and no thin space introduced around a word break.
    #[test]
    fn a_label_is_upper_case_and_letter_spaced() {
        assert_eq!(
            spaced_caps("elapsed"),
            "E\u{2009}L\u{2009}A\u{2009}P\u{2009}S\u{2009}E\u{2009}D"
        );
    }

    /// A real space already separates the words. Letter-spacing it too would
    /// set the word gap twice as wide as it should be.
    #[test]
    fn a_word_break_is_not_spaced_as_well() {
        assert_eq!(spaced_caps("a b"), "A B");
        assert_eq!(spaced_caps("to do"), "T\u{2009}O D\u{2009}O");
    }

    #[test]
    fn an_empty_label_stays_empty() {
        assert_eq!(spaced_caps(""), "");
    }
}
