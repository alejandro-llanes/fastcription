//! The app's palette, and its mapping onto `egui::Visuals`.
//!
//! ARCHITECTURE.md ties fastcription to `fastframe-theme` specifically because
//! this user runs Omarchy: `Catalog::enable_desktop_themes` makes the app
//! follow Omarchy's current theme the same way the rest of the desktop does,
//! with no setting of our own to keep in sync.

use egui::{Color32, Stroke, Visuals};
use fastframe_theme::{Base, Palette as ThemePalette};

/// The contrast the colours that carry words have to clear.
///
/// This app exists for someone who reads an English meeting because they
/// cannot follow it by ear, so the transcript is not decoration they can skip —
/// it is the only channel they have. WCAG AA asks 4.5:1 of body text and AAA
/// asks 7:1; the palette holds its primary text to AAA and anything else that
/// spells out words to AA. [`Palette::dim`] is held to the 3:1 non-text
/// minimum precisely because nothing may draw words with it.
pub const TEXT_CONTRAST: f32 = 7.0;
pub const WORDS_CONTRAST: f32 = 4.5;
pub const DECORATION_CONTRAST: f32 = 3.0;

/// The sixteen colours every fastframe palette file understands
/// ([`fastframe_theme::BASE_COLORS`]). fastcription has no app-specific
/// colours yet, so [`ThemePalette::derive`] needs no override.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    pub dark: bool,
    pub window: Color32,
    pub panel: Color32,
    pub surface: Color32,
    pub surface_hover: Color32,
    pub surface_active: Color32,
    pub outline: Color32,
    pub text: Color32,
    pub secondary: Color32,
    pub dim: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    pub on_accent: Color32,
    pub danger: Color32,
    pub warning: Color32,
    pub overlay: Color32,
    pub shadow: Color32,
}

impl Palette {
    fn dark_default() -> Self {
        Self {
            dark: true,
            window: Color32::from_rgb(0x1e, 0x1e, 0x2a),
            panel: Color32::from_rgb(0x24, 0x24, 0x33),
            surface: Color32::from_rgb(0x2b, 0x2b, 0x3c),
            surface_hover: Color32::from_rgb(0x34, 0x34, 0x48),
            surface_active: Color32::from_rgb(0x3d, 0x3d, 0x54),
            outline: Color32::from_rgb(0x44, 0x44, 0x5c),
            text: Color32::from_rgb(0xe6, 0xe6, 0xf0),
            secondary: Color32::from_rgb(0xb0, 0xb0, 0xc4),
            dim: Color32::from_rgb(0x7a, 0x7a, 0x90),
            // 0x8a7ae8 measured 4.38:1 against `panel`, just under AA, and the
            // accent spells out words: speaker names, the Info notice, the
            // service pill. Lightening it to 5.47:1 also lifts `on_accent` on
            // an accent fill from 5.34:1 to 6.66:1.
            accent: Color32::from_rgb(0x9c, 0x8e, 0xee),
            accent_hover: Color32::from_rgb(0xae, 0xa2, 0xf5),
            on_accent: Color32::from_rgb(0x12, 0x12, 0x1c),
            danger: Color32::from_rgb(0xe0, 0x6c, 0x75),
            warning: Color32::from_rgb(0xe5, 0xc0, 0x7b),
            overlay: Color32::from_black_alpha(190),
            shadow: Color32::from_black_alpha(120),
        }
    }

    fn light_default() -> Self {
        Self {
            dark: false,
            window: Color32::from_rgb(0xfb, 0xfb, 0xfd),
            panel: Color32::from_rgb(0xf1, 0xf1, 0xf6),
            surface: Color32::from_rgb(0xe8, 0xe8, 0xef),
            surface_hover: Color32::from_rgb(0xdf, 0xdf, 0xe9),
            surface_active: Color32::from_rgb(0xd3, 0xd3, 0xe1),
            outline: Color32::from_rgb(0xc7, 0xc7, 0xd6),
            text: Color32::from_rgb(0x1c, 0x1c, 0x26),
            secondary: Color32::from_rgb(0x50, 0x50, 0x63),
            // 0x8a8a9c measured 3.01:1 against `panel` and 2.78:1 against
            // `surface` — under the 3:1 floor on the surface that the sidebar
            // and the transcript rows actually sit on.
            dim: Color32::from_rgb(0x7c, 0x7c, 0x90),
            accent: Color32::from_rgb(0x6b, 0x56, 0xd6),
            accent_hover: Color32::from_rgb(0x5a, 0x47, 0xc0),
            on_accent: Color32::from_rgb(0xfa, 0xfa, 0xff),
            danger: Color32::from_rgb(0xc4, 0x3d, 0x47),
            // 0x9a730e measured 3.86:1 against `panel`. The warning colour
            // carries sentences — "transcription behind", the unencrypted-
            // address warning — so it has to clear AA like the rest of them.
            warning: Color32::from_rgb(0x8a, 0x64, 0x00),
            overlay: Color32::from_black_alpha(90),
            shadow: Color32::from_black_alpha(40),
        }
    }

    /// Raises any colour that spells out words far enough to be readable
    /// against the panel it is drawn on.
    ///
    /// The defaults above already clear their thresholds, and a test holds them
    /// there — but a palette does not have to come from here. With
    /// `enable_desktop_themes`, fastcription draws in whatever Omarchy theme
    /// the desktop is wearing, and a palette written for a terminal emulator
    /// can easily put `secondary` at 2:1 against its own background. For the
    /// one audience this app has — someone who reads a meeting because they
    /// cannot follow it by ear — an unreadable transcript is not a cosmetic
    /// problem.
    ///
    /// Only colours that carry words are touched, only when they fall short,
    /// and only as far as their threshold: the theme keeps its hue, which is
    /// the whole point of following the desktop in the first place.
    pub fn enforce_contrast(&mut self) {
        for (color, floor) in [
            (&mut self.text, TEXT_CONTRAST),
            (&mut self.secondary, WORDS_CONTRAST),
            (&mut self.accent, WORDS_CONTRAST),
            (&mut self.warning, WORDS_CONTRAST),
            (&mut self.danger, WORDS_CONTRAST),
            // Decoration rather than words — a separator, the meter — but a
            // hairline nobody can see is a hairline that is not there.
            (&mut self.dim, DECORATION_CONTRAST),
        ] {
            *color = readable(*color, self.panel, floor);
        }
    }

    /// Maps the sixteen colours onto an `egui::Visuals` and applies it to
    /// **both** of egui's theme slots.
    ///
    /// `set_visuals` only fills the slot for the theme currently in use, and
    /// egui follows the desktop's light/dark preference: a desktop that
    /// flipped to light while fastcription was running fell through to stock
    /// egui light, which shares nothing with the user's theme. The palette
    /// already is whichever one fastframe resolved for the desktop, so the
    /// right answer is to put it in both slots and let the flip change
    /// nothing.
    ///
    /// Widgets that need a colour `Visuals` has no slot for (notice colours,
    /// provisional-segment dimming) read the palette directly.
    pub fn apply(&self, ctx: &egui::Context) {
        let visuals = self.visuals();
        ctx.set_visuals_of(egui::Theme::Dark, visuals.clone());
        ctx.set_visuals_of(egui::Theme::Light, visuals);
    }

    fn visuals(&self) -> Visuals {
        let mut visuals = if self.dark {
            Visuals::dark()
        } else {
            Visuals::light()
        };
        visuals.panel_fill = self.panel;
        visuals.window_fill = self.window;
        visuals.extreme_bg_color = self.surface;
        visuals.faint_bg_color = self.surface_hover;
        visuals.code_bg_color = self.surface;
        visuals.hyperlink_color = self.accent;
        visuals.warn_fg_color = self.warning;
        visuals.error_fg_color = self.danger;
        visuals.selection.bg_fill = self.accent;
        visuals.selection.stroke = Stroke::new(1.0, self.on_accent);
        visuals.widgets.noninteractive.bg_fill = self.panel;
        visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, self.text);
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, self.outline);
        visuals.widgets.inactive.bg_fill = self.surface;
        visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, self.text);
        visuals.widgets.hovered.bg_fill = self.surface_hover;
        visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, self.text);
        visuals.widgets.active.bg_fill = self.surface_active;
        // `on_accent` is the colour for text *on* the accent fill, and this
        // fill is `surface_active`. On the dark default that made a pressed
        // button's label near-black on dark grey — the label vanished for as
        // long as the mouse was down.
        visuals.widgets.active.fg_stroke = Stroke::new(1.0, self.text);
        visuals.widgets.active.bg_stroke = Stroke::new(1.0, self.accent);
        // An open combo box or menu has its own slot, which nothing filled:
        // every dropdown in the app was drawn in stock egui's grey.
        visuals.widgets.open.bg_fill = self.surface_active;
        visuals.widgets.open.fg_stroke = Stroke::new(1.0, self.text);
        visuals.widgets.open.bg_stroke = Stroke::new(1.0, self.outline);
        visuals.override_text_color = Some(self.text);
        visuals
    }
}

/// WCAG 2.1 relative luminance of an opaque sRGB colour.
///
/// Spelled out rather than approximated with a brightness average, because the
/// average is wrong in exactly the direction that matters here: it rates a
/// saturated mid-tone far brighter than the eye reads it, which is how a
/// palette ends up with unreadable text that measured fine.
pub fn relative_luminance(color: Color32) -> f32 {
    fn channel(byte: u8) -> f32 {
        let c = f32::from(byte) / 255.0;
        if c <= 0.039_28 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
}

/// WCAG contrast ratio between two opaque colours, from 1:1 (identical) to
/// 21:1 (black on white). Symmetric: the order of the arguments is irrelevant.
pub fn contrast_ratio(a: Color32, b: Color32) -> f32 {
    let (a, b) = (relative_luminance(a), relative_luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// `color` if it already clears `floor` against `behind`, else the nearest
/// step towards white or black that does.
///
/// Interpolated in gamma space towards whichever end can raise the ratio at
/// all — there is only ever one — so the result keeps the hue it started with
/// and simply has more or less of it. Sixteen steps is finer than the eye reads
/// as a different colour and coarse enough to stop immediately.
fn readable(color: Color32, behind: Color32, floor: f32) -> Color32 {
    if contrast_ratio(color, behind) >= floor {
        return color;
    }
    let toward = if relative_luminance(behind) < 0.18 {
        Color32::WHITE
    } else {
        Color32::BLACK
    };
    const STEPS: u32 = 16;
    let mut lifted = toward;
    for step in 1..=STEPS {
        lifted = color.lerp_to_gamma(toward, step as f32 / STEPS as f32);
        if contrast_ratio(lifted, behind) >= floor {
            break;
        }
    }
    lifted
}

impl ThemePalette for Palette {
    fn base(base: Base) -> Self {
        match base {
            Base::Dark => Self::dark_default(),
            Base::Light => Self::light_default(),
        }
    }

    fn set(&mut self, name: &str, color: Color32) -> bool {
        match name {
            "window" => self.window = color,
            "panel" => self.panel = color,
            "surface" => self.surface = color,
            "surface_hover" => self.surface_hover = color,
            "surface_active" => self.surface_active = color,
            "outline" => self.outline = color,
            "text" => self.text = color,
            "secondary" => self.secondary = color,
            "dim" => self.dim = color,
            "accent" => self.accent = color,
            "accent_hover" => self.accent_hover = color,
            "on_accent" => self.on_accent = color,
            "danger" => self.danger = color,
            "warning" => self.warning = color,
            "overlay" => self.overlay = color,
            "shadow" => self.shadow = color,
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{
        contrast_ratio, readable, relative_luminance, Palette, DECORATION_CONTRAST, TEXT_CONTRAST,
        WORDS_CONTRAST,
    };
    use egui::Color32;
    use fastframe_theme::{Base, Palette as ThemePalette};

    /// The two ends of the scale and one published value in between, so a
    /// transcription slip in the gamma expansion cannot pass.
    #[test]
    fn the_ratio_matches_the_published_arithmetic() {
        assert!((relative_luminance(Color32::WHITE) - 1.0).abs() < 1e-4);
        assert!(relative_luminance(Color32::BLACK).abs() < 1e-6);
        assert!((contrast_ratio(Color32::WHITE, Color32::BLACK) - 21.0).abs() < 0.01);
        assert!((contrast_ratio(Color32::WHITE, Color32::WHITE) - 1.0).abs() < 1e-5);
        // #767676 on white is the canonical "exactly AA" grey: 4.54:1.
        let aa_grey = Color32::from_rgb(0x76, 0x76, 0x76);
        let ratio = contrast_ratio(aa_grey, Color32::WHITE);
        assert!((ratio - 4.54).abs() < 0.02, "got {ratio}");
    }

    #[test]
    fn the_ratio_does_not_depend_on_which_colour_is_named_first() {
        let (a, b) = (Color32::from_rgb(0x12, 0x34, 0x56), Color32::WHITE);
        assert_eq!(contrast_ratio(a, b), contrast_ratio(b, a));
    }

    /// The audit that caught the original palette: `dim` was being used for the
    /// provisional tail of the live transcript — the newest words, the ones the
    /// reader most needs — at 3.0:1 on the light base, which is below AA for
    /// text. The fix was both halves: stop drawing words in `dim`, and move the
    /// base values that fell short.
    ///
    /// Measured against `panel` and `window`, the two fills words are drawn on
    /// (panels everywhere, and the window itself in compact mode) rather than
    /// against `panel` alone, since a colour that passes on one and fails on
    /// the other fails where it is read.
    #[test]
    fn every_colour_that_carries_words_is_readable_on_both_bases() {
        for base in [Base::Dark, Base::Light] {
            let palette: Palette = ThemePalette::base(base);
            for (name, behind) in [("panel", palette.panel), ("window", palette.window)] {
                let check = |what: &str, color: Color32, floor: f32| {
                    let ratio = contrast_ratio(color, behind);
                    assert!(
                        ratio >= floor,
                        "{base:?}: {what} on {name} is {ratio:.2}:1, needs {floor}:1"
                    );
                };
                check("text", palette.text, TEXT_CONTRAST);
                check("secondary", palette.secondary, WORDS_CONTRAST);
                // The three notice colours spell out whole sentences.
                check("accent", palette.accent, WORDS_CONTRAST);
                check("warning", palette.warning, WORDS_CONTRAST);
                check("danger", palette.danger, WORDS_CONTRAST);
                // Never words: separators, the level meter, a hairline.
                check("dim", palette.dim, DECORATION_CONTRAST);
            }
            // A pressed accent button draws `on_accent` on the accent fill.
            let on_accent = contrast_ratio(palette.on_accent, palette.accent);
            assert!(
                on_accent >= WORDS_CONTRAST,
                "{base:?}: on_accent over accent is {on_accent:.2}:1"
            );
        }
    }

    /// A theme from the desktop is not held to anything, so the repair has to
    /// work on a palette that is actively hostile — here, one whose text is its
    /// own background.
    #[test]
    fn an_unreadable_theme_is_lifted_until_it_can_be_read() {
        for base in [Base::Dark, Base::Light] {
            let mut palette: Palette = ThemePalette::base(base);
            palette.text = palette.panel;
            palette.secondary = palette.panel;
            palette.dim = palette.panel;
            palette.enforce_contrast();

            assert!(contrast_ratio(palette.text, palette.panel) >= TEXT_CONTRAST);
            assert!(contrast_ratio(palette.secondary, palette.panel) >= WORDS_CONTRAST);
            assert!(contrast_ratio(palette.dim, palette.panel) >= DECORATION_CONTRAST);
        }
    }

    /// The repair must be a no-op on anything that already passes: a theme that
    /// is readable keeps every colour the user chose.
    #[test]
    fn a_readable_theme_is_left_exactly_as_it_is() {
        for base in [Base::Dark, Base::Light] {
            let palette: Palette = ThemePalette::base(base);
            let mut repaired = palette.clone();
            repaired.enforce_contrast();
            assert_eq!(palette, repaired, "{base:?}");
        }
    }

    /// Only ever away from the background, never past it: lifting a colour on a
    /// light panel has to darken it, and on a dark panel lighten it.
    #[test]
    fn lifting_moves_away_from_the_background_it_is_drawn_on() {
        // Each grey is one that fails against the background it is paired
        // with, which is the only case the repair acts on.
        let dark = Color32::from_rgb(0x20, 0x20, 0x20);
        let too_dark = Color32::from_rgb(0x3a, 0x3a, 0x3a);
        let on_dark = readable(too_dark, dark, WORDS_CONTRAST);
        assert!(relative_luminance(on_dark) > relative_luminance(too_dark));
        assert!(contrast_ratio(on_dark, dark) >= WORDS_CONTRAST);

        let light = Color32::from_rgb(0xf0, 0xf0, 0xf0);
        let too_light = Color32::from_rgb(0xaa, 0xaa, 0xaa);
        let on_light = readable(too_light, light, WORDS_CONTRAST);
        assert!(relative_luminance(on_light) < relative_luminance(too_light));
        assert!(contrast_ratio(on_light, light) >= WORDS_CONTRAST);
    }
}
