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
            for segment in &app.segments {
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

fn timestamp(start_ms: u64) -> String {
    let total_secs = start_ms / 1000;
    format!("{:02}:{:02}", total_secs / 60, total_secs % 60)
}
