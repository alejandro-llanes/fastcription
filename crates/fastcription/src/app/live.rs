//! The live transcript: committed segments scroll up from the bottom and
//! stay stuck there unless the user scrolls away — `stick_to_bottom`, egui's
//! own implementation of the behaviour ARCHITECTURE.md calls the single
//! most-used one in the app. Provisional segments render dimmed and italic;
//! a `Segment::translation`, once something produces one, renders as a
//! second line under its segment (decision D4).

use egui::RichText;
use fc_core::{Pressure, Segment, Track};

use crate::app::App;
use crate::i18n::t;

/// How many of the most recent segments the live view draws.
///
/// egui lays out every child of a scroll area each frame, and wrapped text
/// cannot use the uniform-row fast path. A three-hour meeting produces well
/// over a thousand segments, which would cost real frame time for lines nobody
/// is looking at — the live view is for following along, and the whole
/// transcript is a click away in the conversation's history.
const LIVE_WINDOW: usize = 400;

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.heading(t("Live transcript"));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if app.pressure == Pressure::Lagging {
                ui.colored_label(
                    app.palette.warning,
                    t("transcription behind — chunks are growing"),
                );
            }
        });
    });
    ui.separator();

    egui::ScrollArea::vertical()
        .id_salt("live-transcript-scroll")
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            if app.segments.is_empty() && app.provisional.is_empty() {
                ui.weak(t("Nothing transcribed yet. Press Start to begin."));
                return;
            }
            let hidden = app.segments.len().saturating_sub(LIVE_WINDOW);
            if hidden > 0 {
                ui.weak(format!(
                    "{hidden} {}",
                    t("earlier lines — open this conversation in the sidebar to read them all")
                ));
                ui.add_space(4.0);
            }
            for segment in app.segments.iter().skip(hidden) {
                row(ui, &app.palette, segment);
            }
            let mut pending: Vec<&Segment> = app.provisional.values().collect();
            pending.sort_by_key(|segment| segment.seq);
            for segment in pending {
                row(ui, &app.palette, segment);
            }
        });
}

fn row(ui: &mut egui::Ui, palette: &crate::theme::Palette, segment: &Segment) {
    ui.horizontal_wrapped(|ui| {
        let speaker_color = match segment.track {
            Track::Selected => palette.accent,
            Track::Microphone => palette.secondary,
        };
        ui.label(
            RichText::new(segment.speaker_label())
                .color(speaker_color)
                .strong(),
        );
        ui.label(
            RichText::new(timestamp(segment.start_ms))
                .color(palette.dim)
                .small(),
        );
        let text = if segment.provisional {
            RichText::new(&segment.text).italics().color(palette.dim)
        } else {
            RichText::new(&segment.text).color(palette.text)
        };
        ui.label(text);
    });
    // Room for the translation line, whenever something fills it in.
    if let Some(translation) = &segment.translation {
        ui.horizontal(|ui| {
            ui.add_space(24.0);
            ui.label(
                RichText::new(translation)
                    .italics()
                    .color(palette.secondary),
            );
        });
    }
    ui.add_space(4.0);
}

/// `mm:ss`, growing to `h:mm:ss` past an hour rather than counting minutes
/// into three digits.
fn timestamp(start_ms: u64) -> String {
    let total_secs = start_ms / 1000;
    let (hours, minutes, seconds) = (total_secs / 3600, (total_secs % 3600) / 60, total_secs % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::timestamp;

    #[test]
    fn timestamps_grow_an_hour_field_instead_of_counting_to_ninety_minutes() {
        assert_eq!(timestamp(0), "00:00");
        assert_eq!(timestamp(9_000), "00:09");
        assert_eq!(timestamp(61_000), "01:01");
        assert_eq!(timestamp(3_599_000), "59:59");
        assert_eq!(timestamp(3_600_000), "1:00:00");
        assert_eq!(timestamp(5_400_000), "1:30:00");
        assert_eq!(timestamp(36_000_000), "10:00:00");
    }
}
