//! The app's palette, and its mapping onto `egui::Visuals`.
//!
//! ARCHITECTURE.md ties fastcription to `fastframe-theme` specifically because
//! this user runs Omarchy: `Catalog::enable_desktop_themes` makes the app
//! follow Omarchy's current theme the same way the rest of the desktop does,
//! with no setting of our own to keep in sync.

use egui::{Color32, Stroke, Visuals};
use fastframe_theme::{Base, Palette as ThemePalette};

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
            accent: Color32::from_rgb(0x8a, 0x7a, 0xe8),
            accent_hover: Color32::from_rgb(0x9c, 0x8e, 0xee),
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
            dim: Color32::from_rgb(0x8a, 0x8a, 0x9c),
            accent: Color32::from_rgb(0x6b, 0x56, 0xd6),
            accent_hover: Color32::from_rgb(0x5a, 0x47, 0xc0),
            on_accent: Color32::from_rgb(0xfa, 0xfa, 0xff),
            danger: Color32::from_rgb(0xc4, 0x3d, 0x47),
            warning: Color32::from_rgb(0x9a, 0x73, 0x0e),
            overlay: Color32::from_black_alpha(90),
            shadow: Color32::from_black_alpha(40),
        }
    }

    /// Maps the sixteen colours onto an `egui::Visuals` and applies it.
    /// Widgets that need a colour `Visuals` has no slot for (the danger
    /// banner, provisional-segment dimming) read the palette directly.
    pub fn apply(&self, ctx: &egui::Context) {
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
        visuals.widgets.active.fg_stroke = Stroke::new(1.0, self.on_accent);
        visuals.override_text_color = Some(self.text);
        ctx.set_visuals(visuals);
    }
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
