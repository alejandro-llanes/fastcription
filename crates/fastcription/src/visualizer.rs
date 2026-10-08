//! The audio visualiser: the spectrum from `fc_audio::spectrum`, drawn.
//!
//! Two jobs, and the second is the reason this exists at all. The obvious one
//! is decoration. The useful one is that it answers "is this thing hearing
//! anything?" at a glance and from across a room — the question a user asks
//! when the transcript has not moved for twenty seconds and they cannot tell
//! whether the meeting went quiet, the source is wrong, or the app has died.
//! A percentage on a progress bar answers it too, but only to someone close
//! enough to read a number.
//!
//! ## Why it eases
//!
//! Bands arrive twenty times a second and the window draws sixty. Drawing the
//! last value received would show each one for three frames and then jump,
//! which reads as a stutter rather than as sound. Every style therefore draws
//! `shown`, which chases `target` on a time constant rather than a per-frame
//! fraction — the same easing has to look the same whether the compositor is
//! giving us 60 Hz or 144.
//!
//! ## Why the colours are the theme's
//!
//! The app follows Omarchy's current theme (`theme.rs`), so the visualiser
//! cannot have colours of its own without being the one thing on screen that
//! ignores the desktop. The two-tone gradient the reference designs get from
//! picking two neon hues is got here by rotating the theme's own accent
//! around the colour wheel, which keeps the palette's identity and still
//! gives the low and high ends of the spectrum visibly different colours.

use std::time::Instant;

use egui::{Color32, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, Vec2};
use fc_core::SPECTRUM_BANDS as BANDS;

use crate::i18n::t;
use crate::theme::Palette;

/// How quickly `shown` chases `target`, as a time constant in seconds.
///
/// Bands arrive every 50 ms, so this has to cover most of the distance inside
/// that or the visualiser lags the sound; and it must not cover all of it, or
/// there was no point easing. 35 ms is about three quarters of the way between
/// updates.
const EASE_TAU: f32 = 0.035;

/// Seconds for the whole display to sink to nothing once audio stops.
///
/// Slower than [`EASE_TAU`] on purpose: this is the end of a recording rather
/// than a gap between words, and a visualiser that snaps flat the instant Stop
/// is pressed looks like a crash.
const IDLE_TAU: f32 = 0.25;

/// How long the display's reference level takes to fall, in seconds.
///
/// The bands are an honest measurement and honest measurements of a meeting
/// are *quiet*: a monitor at a normal system volume puts speech around
/// -50 dBFS in any one band, which is a fifth of the way up the bar. Drawn
/// literally the visualiser is a row of stubs whatever is happening, and the
/// shape — the part actually worth looking at — is squashed into the bottom
/// of the strip.
///
/// So the display auto-ranges, the way a meter with no fixed scale does. It
/// tracks the loudest band it has seen recently and scales to that, which
/// makes a quiet room and a loud one both fill the strip and leaves the shape
/// the same either way. The reference rises instantly and falls on this time
/// constant, so one loud syllable does not shrink everything else for the
/// next second.
const GAIN_TAU: f32 = 2.5;

/// The quietest reference the display will scale to.
///
/// Without a floor, silence divides by nothing and the room's noise is
/// amplified into a light show. This caps the gain at about eight times,
/// which is the difference between a quiet meeting and a loud one, not the
/// difference between silence and a cough.
const GAIN_FLOOR: f32 = 0.12;

/// Where the loudest band sits once the display has settled on its range.
///
/// Not 1.0: a band that is always exactly at the ceiling has nowhere to go
/// when someone actually raises their voice.
const GAIN_TARGET: f32 = 0.92;

/// Roughly how far apart bars are placed, in points.
///
/// Bars are not drawn one per band. Across the footer's full span that would
/// be eighty points of bar apiece — a row of boxes rather than a spectrum —
/// and in the 140-point fallback slot it would be four. Columns are spaced at
/// about this pitch instead and their heights interpolated from the bands, so
/// the same style reads correctly at every width it is drawn at.
const BAR_PITCH: f32 = 12.0;

/// Fewest and most bars, whatever the width works out to.
///
/// The floor keeps a narrow strip from becoming three fat blocks; the ceiling
/// is where more bars stop being distinguishable and start being a fill.
const BAR_COUNT: std::ops::RangeInclusive<usize> = 8..=160;

/// Below this width a bar is too thin for a halo to read as anything but a
/// smudge, so it is not drawn.
const GLOW_MIN_BAR: f32 = 6.0;

/// Columns per band when a style draws a continuous shape rather than bars.
///
/// The curve styles interpolate between band values; this is how finely. Four
/// is enough that the eye reads a curve instead of a polygon at the widths the
/// visualiser is drawn at, and cheap enough to redraw every frame.
const SUBDIVISIONS: usize = 4;

/// How far around the colour wheel the top of the spectrum sits from the
/// bottom.
///
/// Enough that low and high read as different colours, not so much that the
/// result stops looking like the theme's accent. The reference designs run
/// magenta to cyan, which is about this far apart.
const HUE_SPREAD: f32 = 80.0;

/// Which shape the spectrum is drawn as.
///
/// A setting rather than a choice made here: this is the one piece of the
/// window whose only job is to be looked at, and taste in it varies more than
/// anything else in the app. `Off` is a first-class option for the same reason
/// — someone reading captions during a meeting they are struggling to follow
/// may well not want anything else moving on the screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum Style {
    /// Classic analyser bars, rising from the floor. The default: it is the
    /// one shape everyone has already learned to read as sound.
    #[default]
    Bars,
    /// Bars mirrored about the centre line.
    Mirror,
    /// A filled, symmetrical waveform.
    Wave,
    /// Overlapping translucent curves that drift against each other.
    Ribbon,
    /// A ring whose edge is pushed out by the spectrum.
    Ring,
    /// Nothing at all.
    Off,
}

impl Style {
    /// Every style, in the order the settings pane offers them.
    pub const ALL: [Style; 6] = [
        Style::Bars,
        Style::Mirror,
        Style::Wave,
        Style::Ribbon,
        Style::Ring,
        Style::Off,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Style::Bars => t("Bars"),
            Style::Mirror => t("Mirrored"),
            Style::Wave => t("Waveform"),
            Style::Ribbon => t("Ribbons"),
            Style::Ring => t("Ring"),
            Style::Off => t("Off"),
        }
    }

    /// Whether this style draws anything, so a caller can skip reserving space
    /// for it.
    pub fn visible(self) -> bool {
        self != Style::Off
    }
}

/// The drawing state: the last spectrum received, what is currently on screen,
/// and how long it has been since the last frame.
#[derive(Debug)]
pub struct Visualizer {
    target: [f32; BANDS],
    shown: [f32; BANDS],
    /// Seconds since the first frame, for styles that move on their own.
    phase: f32,
    /// `None` until the first frame, so the first `dt` is not the time since
    /// the process started.
    last: Option<Instant>,
    /// The loudest band seen recently, which the display scales to. See
    /// [`GAIN_TAU`].
    reference: f32,
    /// Set by [`Self::idle`]; makes the decay slow rather than instant.
    idling: bool,
}

/// A visualiser that has never been fed is idle by definition — not merely
/// silent. The distinction matters because it picks the decay rate, and
/// deriving this would start every launch in the fast one.
impl Default for Visualizer {
    fn default() -> Self {
        Self {
            target: [0.0; BANDS],
            shown: [0.0; BANDS],
            phase: 0.0,
            last: None,
            reference: 0.0,
            idling: true,
        }
    }
}

impl Visualizer {
    /// A fresh spectrum from the capture thread.
    pub fn feed(&mut self, bands: [f32; BANDS]) {
        self.target = bands;
        self.idling = false;
    }

    /// Nothing is being captured. The display sinks rather than cutting out.
    pub fn idle(&mut self) {
        self.target = [0.0; BANDS];
        self.idling = true;
    }

    /// Whether the next frame would look different from this one.
    ///
    /// This is what decides whether to keep asking for repaints, so both
    /// halves matter: a display still converging on a new spectrum has to
    /// carry on, and so does one sitting at a steady non-zero level, because
    /// the styles that drift do so on their own clock. Both are false only
    /// once everything has decayed to nothing, which is when a window showing
    /// no audio should stop redrawing entirely.
    pub fn animating(&self) -> bool {
        self.shown
            .iter()
            .zip(self.target.iter())
            .any(|(shown, target)| (shown - target).abs() > 0.001)
            || self.shown.iter().any(|&v| v > 0.001)
    }

    /// Advances the easing. Called once per frame, before drawing.
    fn step(&mut self, now: Instant) {
        let dt = match self.last.replace(now) {
            // A frame longer than a quarter second means the window was not
            // being drawn — hidden to the tray, or the compositor was busy.
            // Easing across that gap in one step would be a visible jump.
            Some(previous) => (now - previous).as_secs_f32().min(0.25),
            None => 0.0,
        };
        self.phase += dt;
        let tau = if self.idling { IDLE_TAU } else { EASE_TAU };
        // Frame-rate independent exponential approach: the same wall-clock
        // time covers the same fraction of the distance at any frame rate.
        let k = 1.0 - (-dt / tau).exp();
        for (shown, &target) in self.shown.iter_mut().zip(self.target.iter()) {
            *shown += (target - *shown) * k;
        }

        // Instant up, slow down: the reference is what the display divides by,
        // and a reference that fell as fast as the audio would simply undo the
        // auto-ranging on every gap between words.
        let loudest = self.shown.iter().copied().fold(0.0, f32::max);
        if loudest >= self.reference {
            self.reference = loudest;
        } else {
            self.reference += (loudest - self.reference) * (1.0 - (-dt / GAIN_TAU).exp());
        }
    }

    /// What the display multiplies the bands by, so the loudest recent one
    /// lands near the top of the strip.
    fn gain(&self) -> f32 {
        GAIN_TARGET / self.reference.max(GAIN_FLOOR)
    }

    /// Draws the visualiser at `size`, or across the full available width when
    /// `size.x` is zero.
    ///
    /// `None` when the style is `Off`, so a caller can lay out as though the
    /// visualiser were not there rather than leaving a gap where it would have
    /// been. The response is returned for its hover text and so a caller can
    /// make it a target.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        palette: &Palette,
        style: Style,
        size: Vec2,
    ) -> Option<Response> {
        if !style.visible() {
            return None;
        }
        let width = if size.x > 0.0 {
            size.x
        } else {
            ui.available_width()
        };
        let (rect, response) = ui.allocate_exact_size(Vec2::new(width, size.y), Sense::hover());
        self.paint_at(ui, rect, palette, style);
        Some(response)
    }

    /// Draws into a rect the caller already owns, allocating nothing.
    ///
    /// The status bar draws the spectrum full-bleed as its own background and
    /// lays a translucent bar over the lower part of it, which needs the whole
    /// strip rather than a slot reserved inside it.
    pub fn paint_at(&mut self, ui: &Ui, rect: Rect, palette: &Palette, style: Style) {
        if !style.visible() || !ui.is_rect_visible(rect) {
            return;
        }
        // Stepping twice in one frame is harmless — the second call measures a
        // `dt` of almost nothing and moves nothing — so a caller that draws the
        // same visualiser in two places does not have to coordinate.
        self.step(Instant::now());
        // Without this the window only redraws when something else asks it to,
        // and the easing would advance in jerks whenever an event happened to
        // arrive. It stops once everything has decayed, so an idle window is
        // not held at 60 fps for a row of flat bars.
        if self.animating() {
            ui.ctx().request_repaint();
        }
        self.paint(ui, rect, palette, style);
    }

    fn paint(&self, ui: &Ui, rect: Rect, palette: &Palette, style: Style) {
        let painter = ui.painter_at(rect);
        match style {
            Style::Bars => self.bars(&painter, rect, palette, false),
            Style::Mirror => self.bars(&painter, rect, palette, true),
            Style::Wave => self.wave(&painter, rect, palette),
            Style::Ribbon => self.ribbon(&painter, rect, palette),
            Style::Ring => self.ring(&painter, rect, palette),
            Style::Off => {}
        }
    }

    /// Band `i`'s colour: the theme's accent, rotated further round the wheel
    /// the higher the frequency.
    fn tint(&self, palette: &Palette, fraction: f32) -> Color32 {
        crate::theme::shift_hue(palette.accent, fraction * HUE_SPREAD)
    }

    /// The spectrum sampled at `t` in 0..1, interpolating between bands and
    /// scaled by the display's current range.
    ///
    /// Every style reads the spectrum through here, so the auto-ranging
    /// applies to all of them and none of them has to know about it. Silence
    /// stays silent: the gain multiplies zero by a larger number and gets
    /// zero.
    fn sample(&self, t: f32) -> f32 {
        let position = t.clamp(0.0, 1.0) * (BANDS - 1) as f32;
        let low = position.floor() as usize;
        let high = (low + 1).min(BANDS - 1);
        let blend = position - low as f32;
        let raw = self.shown[low] * (1.0 - blend) + self.shown[high] * blend;
        (raw * self.gain()).min(1.0)
    }

    /// Analyser bars, optionally mirrored about the centre line.
    fn bars(&self, painter: &egui::Painter, rect: Rect, palette: &Palette, mirrored: bool) {
        let columns = ((rect.width() / BAR_PITCH).round() as usize)
            .clamp(*BAR_COUNT.start(), *BAR_COUNT.end());
        let slot = rect.width() / columns as f32;
        // A quarter of each slot is the gap. Narrower and the bars merge into
        // a solid block at the widths this is drawn at; wider and there is
        // more gap than bar.
        let bar = (slot * 0.75).max(1.0);
        let glow = bar >= GLOW_MIN_BAR;
        for column in 0..columns {
            let t = column as f32 / (columns - 1).max(1) as f32;
            let value = self.sample(t);
            let tint = self.tint(palette, t);
            let x = rect.left() + slot * column as f32 + (slot - bar) / 2.0;
            // Every bar keeps a visible stub at silence. A row of bars that
            // vanishes completely looks like the visualiser broke, where a
            // flat row reads as "listening, nothing to hear".
            let extent = (value.max(0.02) * rect.height()).max(2.0);
            let bar_rect = if mirrored {
                let half = extent / 2.0;
                Rect::from_min_max(
                    Pos2::new(x, rect.center().y - half),
                    Pos2::new(x + bar, rect.center().y + half),
                )
            } else {
                Rect::from_min_max(
                    Pos2::new(x, rect.bottom() - extent),
                    Pos2::new(x + bar, rect.bottom()),
                )
            };
            // Rounded, but never more than half the bar's own height: at a
            // full corner radius a quiet bar is shorter than its own corners
            // and comes out a capsule, so a silent strip reads as a dotted
            // line rather than as a row of bars at rest.
            let radius = (bar * 0.3).min(extent * 0.4);
            if glow {
                // egui has no blur, and a larger translucent rounded rectangle
                // behind the solid one is what a blur looks like from a
                // distance.
                painter.rect_filled(
                    bar_rect.expand(bar * 0.35),
                    radius + bar * 0.1,
                    tint.gamma_multiply(0.12 * value.max(0.1)),
                );
            }
            painter.rect_filled(bar_rect, radius, tint);
        }
    }

    /// A filled, symmetrical waveform.
    ///
    /// Drawn as a run of narrow vertical rectangles rather than as one
    /// polygon: the shape is not convex, and egui's tessellator only
    /// guarantees a convex path fills correctly.
    fn wave(&self, painter: &egui::Painter, rect: Rect, palette: &Palette) {
        let columns = BANDS * SUBDIVISIONS;
        let width = rect.width() / columns as f32;
        let centre = rect.center().y;
        for column in 0..columns {
            let t = column as f32 / (columns - 1) as f32;
            let amplitude = self.sample(t);
            let half = (amplitude * rect.height() / 2.0).max(1.0);
            let x = rect.left() + width * column as f32;
            let tint = self.tint(palette, t);
            let column_rect = Rect::from_min_max(
                Pos2::new(x, centre - half),
                Pos2::new(x + width + 1.0, centre + half),
            );
            painter.rect_filled(column_rect, 0.0, tint.gamma_multiply(0.85));
        }
    }

    /// Three translucent curves drifting against each other.
    ///
    /// Each is the same spectrum at a different phase, which is what makes
    /// them separate and move: the reference design's overlapping sine waves,
    /// with the shape coming from the audio rather than from a formula.
    fn ribbon(&self, painter: &egui::Painter, rect: Rect, palette: &Palette) {
        let centre = rect.center().y;
        let points = BANDS * SUBDIVISIONS;
        for layer in 0..3 {
            let drift = self.phase * (0.35 + layer as f32 * 0.22);
            let scale = 1.0 - layer as f32 * 0.22;
            let path: Vec<Pos2> = (0..points)
                .map(|p| {
                    let t = p as f32 / (points - 1) as f32;
                    let amplitude = self.sample(t) * scale;
                    // The travelling sine is what turns a static spectrum into
                    // a ribbon; its own height is the audio's, so a silent
                    // room still leaves a flat line rather than a sine wave
                    // the app invented.
                    let swing = (t * std::f32::consts::TAU * 1.5 + drift).sin();
                    Pos2::new(
                        rect.left() + rect.width() * t,
                        centre + swing * amplitude * rect.height() * 0.42,
                    )
                })
                .collect();
            let tint = self.tint(palette, 0.25 + layer as f32 * 0.3);
            painter.add(Shape::line(
                path,
                Stroke::new(2.0, tint.gamma_multiply(0.75 - layer as f32 * 0.18)),
            ));
        }
    }

    /// A ring pushed outward by the spectrum.
    fn ring(&self, painter: &egui::Painter, rect: Rect, palette: &Palette) {
        let centre = rect.center();
        let base = rect.height().min(rect.width()) * 0.3;
        let reach = rect.height().min(rect.width()) * 0.16;
        let points = BANDS * SUBDIVISIONS;
        let path: Vec<Pos2> = (0..=points)
            .map(|p| {
                let t = p as f32 / points as f32;
                let angle = t * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
                // Mirrored around the circle so the ring closes smoothly: the
                // spectrum runs low-to-high down one side and back up the
                // other, instead of stepping from the last band to the first.
                let folded = if t < 0.5 { t * 2.0 } else { (1.0 - t) * 2.0 };
                let radius = base + self.sample(folded) * reach;
                Pos2::new(
                    centre.x + angle.cos() * radius,
                    centre.y + angle.sin() * radius,
                )
            })
            .collect();
        // A soft disc inside the ring, brightening with the overall level, so
        // the shape reads as something lit from within rather than as an
        // outline. Sized from the quietest part of the edge so it never spills
        // past it.
        // Through `sample`, like the edge, so the core brightens with the
        // scaled display rather than with the raw measurement — otherwise the
        // ring's outline auto-ranges and its glow does not.
        let level = (0..BANDS)
            .map(|i| self.sample(i as f32 / (BANDS - 1) as f32))
            .sum::<f32>()
            / BANDS as f32;
        if level > 0.01 {
            painter.circle_filled(
                centre,
                base * 0.95,
                self.tint(palette, 0.5).gamma_multiply(0.18 * level),
            );
        }
        // Outward from a bright core through two fading halos — the glow of
        // the reference design, built the only way egui can build one.
        for (width, alpha) in [(9.0, 0.10), (5.0, 0.25), (2.0, 1.0)] {
            painter.add(Shape::line(
                path.clone(),
                Stroke::new(width, self.tint(palette, 0.5).gamma_multiply(alpha)),
            ));
        }
    }
}

/// A plausible spectrum, for the settings preview.
///
/// The style picker is useless if every option looks the same, and with
/// nothing being recorded every option *is* the same: a flat line. This is the
/// only place in the app that draws audio nobody made, and the pane that uses
/// it says "Preview — not live audio" underneath for exactly that reason.
///
/// Shaped like speech rather than like noise — most of the energy low, rolling
/// off towards the top — so the preview is a fair picture of what the style
/// will look like in a meeting.
pub fn demo_bands(phase: f32) -> [f32; BANDS] {
    let mut bands = [0.0; BANDS];
    for (i, band) in bands.iter_mut().enumerate() {
        let t = i as f32 / (BANDS - 1) as f32;
        let envelope = (1.0 - t).powf(0.7);
        // Two waves at unrelated rates, so the shape never visibly repeats.
        let wobble = ((phase * 2.1 + t * 7.0).sin() + (phase * 1.3 - t * 4.0).sin()) * 0.5;
        *band = (envelope * (0.55 + 0.45 * wobble)).clamp(0.02, 1.0);
    }
    bands
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_style_has_a_label_and_only_off_draws_nothing() {
        for style in Style::ALL {
            assert!(!style.label().is_empty(), "{style:?}");
            assert_eq!(style.visible(), style != Style::Off, "{style:?}");
        }
    }

    /// `ALL` drives the settings pane, so a style missing from it is a style
    /// the user can never choose.
    #[test]
    fn all_lists_every_style_exactly_once() {
        let mut seen = Style::ALL.to_vec();
        seen.sort_by_key(|s| format!("{s:?}"));
        seen.dedup();
        assert_eq!(seen.len(), Style::ALL.len());
    }

    #[test]
    fn a_fresh_visualiser_is_silent_and_still() {
        let visualizer = Visualizer::default();
        assert!(!visualizer.animating());
        assert_eq!(visualizer.sample(0.0), 0.0);
        assert_eq!(visualizer.sample(1.0), 0.0);
    }

    /// The easing has to be frame-rate independent, or the animation runs at
    /// different speeds on a 60 Hz and a 144 Hz screen. One step of 100 ms and
    /// ten steps of 10 ms must land in the same place.
    #[test]
    fn the_easing_does_not_depend_on_the_frame_rate() {
        let settle = |steps: u32, dt: f32| {
            let mut v = Visualizer {
                target: [1.0; BANDS],
                ..Default::default()
            };
            let mut now = Instant::now();
            v.step(now);
            for _ in 0..steps {
                now += std::time::Duration::from_secs_f32(dt);
                v.step(now);
            }
            v.shown[0]
        };
        let coarse = settle(1, 0.1);
        let fine = settle(10, 0.01);
        assert!(
            (coarse - fine).abs() < 0.02,
            "one 100ms step gave {coarse}, ten 10ms steps gave {fine}"
        );
    }

    /// Interpolation is what the curve styles draw; the ends must be the end
    /// bands exactly, and the middle must lie between its neighbours.
    #[test]
    fn sampling_interpolates_between_bands() {
        let mut shown = [0.0; BANDS];
        shown[0] = 0.0;
        shown[BANDS - 1] = 1.0;
        let visualizer = Visualizer {
            shown,
            ..Default::default()
        };
        assert_eq!(visualizer.sample(0.0), 0.0);
        assert_eq!(visualizer.sample(1.0), 1.0);
        // Out-of-range input is clamped rather than indexing past the end.
        assert_eq!(visualizer.sample(-5.0), 0.0);
        assert_eq!(visualizer.sample(5.0), 1.0);
    }

    #[test]
    fn feeding_clears_idle_and_idling_clears_the_target() {
        let mut visualizer = Visualizer::default();
        visualizer.idle();
        assert!(visualizer.idling);
        visualizer.feed([0.5; BANDS]);
        assert!(!visualizer.idling);
        assert_eq!(visualizer.target, [0.5; BANDS]);
        visualizer.idle();
        assert_eq!(visualizer.target, [0.0; BANDS]);
    }

    /// The preview has to look like something at every instant, or the style
    /// picker shows a flat line at the moment the user happens to look.
    #[test]
    fn the_demo_spectrum_is_always_in_range_and_never_flat() {
        for step in 0..200 {
            let bands = demo_bands(step as f32 * 0.05);
            assert!(
                bands.iter().all(|&b| (0.0..=1.0).contains(&b)),
                "out of range at {step}: {bands:?}"
            );
            let spread = bands.iter().cloned().fold(0.0, f32::max)
                - bands.iter().cloned().fold(1.0, f32::min);
            assert!(spread > 0.1, "flat at {step}: {bands:?}");
        }
    }

    /// The point of auto-ranging: a meeting's bands arrive far below full
    /// scale, and drawn literally they are stubs at the bottom of the strip.
    #[test]
    fn a_quiet_spectrum_is_lifted_towards_the_top() {
        let mut visualizer = Visualizer::default();
        visualizer.feed([0.18; BANDS]);
        let mut now = Instant::now();
        visualizer.step(now);
        for _ in 0..40 {
            now += std::time::Duration::from_millis(25);
            visualizer.step(now);
        }
        let shown = visualizer.sample(0.5);
        assert!(
            shown > 0.8,
            "a steady quiet spectrum should fill the strip, got {shown}"
        );
    }

    /// And the thing auto-ranging must never do: invent a signal. Silence
    /// multiplied by any gain is still silence.
    #[test]
    fn silence_is_not_amplified_into_a_light_show() {
        let mut visualizer = Visualizer::default();
        visualizer.feed([0.0; BANDS]);
        let mut now = Instant::now();
        for _ in 0..40 {
            visualizer.step(now);
            now += std::time::Duration::from_millis(25);
        }
        assert_eq!(visualizer.sample(0.5), 0.0);
        // The gain is capped rather than dividing by nothing.
        assert!(visualizer.gain().is_finite());
        assert!(visualizer.gain() <= GAIN_TARGET / GAIN_FLOOR + 1e-3);
    }

    /// The reference falls slowly so one loud syllable does not shrink
    /// everything else for the next second.
    #[test]
    fn the_reference_falls_slower_than_the_audio() {
        let mut visualizer = Visualizer::default();
        visualizer.feed([0.9; BANDS]);
        let mut now = Instant::now();
        visualizer.step(now);
        for _ in 0..20 {
            now += std::time::Duration::from_millis(25);
            visualizer.step(now);
        }
        let loud = visualizer.reference;
        assert!(loud > 0.8, "setup: expected a loud reference, got {loud}");

        visualizer.feed([0.1; BANDS]);
        now += std::time::Duration::from_millis(200);
        visualizer.step(now);
        assert!(
            visualizer.reference > 0.5,
            "the reference should still be near the recent peak, got {}",
            visualizer.reference
        );
    }

    /// A window that was not drawn for a while — hidden to the tray — must not
    /// have its whole decay collapse into the first frame back.
    #[test]
    fn a_long_gap_between_frames_is_capped() {
        let mut visualizer = Visualizer {
            target: [1.0; BANDS],
            ..Default::default()
        };
        let now = Instant::now();
        visualizer.step(now);
        visualizer.step(now + std::time::Duration::from_secs(60));
        // Capped at 0.25 s, which at EASE_TAU is most of the way but not all.
        assert!(visualizer.shown[0] < 1.0, "{}", visualizer.shown[0]);
    }
}
