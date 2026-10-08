//! The live transcript: committed segments scroll up from the bottom and
//! stay stuck there unless the user scrolls away — `stick_to_bottom`, egui's
//! own implementation of the behaviour ARCHITECTURE.md calls the single
//! most-used one in the app.
//!
//! Rows are drawn by `super::transcript`, shared with the history view, at the
//! size the reader chose. Before anything has been recorded the pane is a
//! readiness checklist instead: this is the first thing a new user looks at,
//! and it used to say "Nothing transcribed yet. Press Start to begin." on a
//! machine where pressing Start could not work.

use egui::RichText;
use fc_core::{Segment, SessionState};

use crate::app::{readiness, transcript, App};
use crate::i18n::{t, tf};

/// How many of the most recent segments the live view draws.
///
/// egui lays out every child of a scroll area each frame, and wrapped text
/// cannot use the uniform-row fast path. A three-hour meeting produces well
/// over a thousand segments, which would cost real frame time for lines nobody
/// is looking at — the live view is for following along, and the whole
/// transcript is a click away in the conversation's history.
const LIVE_WINDOW: usize = 400;

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    // Wrapped, because at the window's minimum width the heading, the size
    // slider and the two copy buttons do not fit on one line.
    ui.horizontal_wrapped(|ui| {
        ui.heading(t("Live transcript"));
        ui.separator();
        ui.add(
            egui::Slider::new(&mut app.settings.transcript_pt, transcript::PT_RANGE)
                .show_value(false)
                .step_by(1.0)
                .text(t("size")),
        )
        .on_hover_text(t(
            "How large the transcript is drawn, here and in compact mode. \
             Ctrl+= and Ctrl+- do the same, and Ctrl+0 goes back to 22.",
        ));
        ui.separator();
        let conversation = app
            .live_conversation
            .and_then(|id| app.conversations.iter().find(|c| c.id == id));
        transcript::copy_buttons(ui, conversation, &app.segments);
    });
    ui.separator();

    let waiting = app.segments.is_empty() && app.provisional.is_empty();
    if waiting && app.state == SessionState::Idle {
        egui::ScrollArea::vertical()
            .id_salt("live-readiness")
            .auto_shrink([false, false])
            .show(ui, |ui| checklist(app, ui));
        return;
    }

    let pt = app.settings.transcript_pt;
    egui::ScrollArea::vertical()
        .id_salt("live-transcript-scroll")
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            if waiting {
                // Nothing said yet. Which of the three reasons it is matters:
                // "listening" under a finished session would have the reader
                // waiting for words that are never coming.
                ui.weak(match app.state {
                    SessionState::Finishing => t("Transcribing the last of the audio…"),
                    SessionState::Paused => t("Paused. Ctrl+Space carries on."),
                    _ => t("Listening. Words appear about two seconds behind the speaker."),
                });
                return;
            }
            let hidden = app.segments.len().saturating_sub(LIVE_WINDOW);
            if hidden > 0 {
                ui.weak(tf(
                    "{} earlier lines — open this conversation in the sidebar to read them all",
                    &[&hidden.to_string()],
                ));
                ui.add_space(4.0);
            }
            for segment in app.segments.iter().skip(hidden) {
                let response = transcript::row(ui, &app.palette, segment, pt);
                transcript::line_menu(&response, segment);
            }
            let mut pending: Vec<&Segment> = app.provisional.values().collect();
            pending.sort_by_key(|segment| segment.seq);
            for segment in pending {
                let response = transcript::row(ui, &app.palette, segment, pt);
                transcript::line_menu(&response, segment);
            }
        });
}

/// What has to be true before Start does anything, and how to make it true.
///
/// Every unmet row carries the command on a button rather than only in a
/// sentence, because the alternative is retyping `voxtype setup --download`
/// from a screenshot.
fn checklist(app: &App, ui: &mut egui::Ui) {
    use crate::icons::Icon;

    let checks = readiness::checks(app);
    if readiness::all_met(&checks) {
        ui.horizontal(|ui| {
            ui.add(Icon::StatusOk.image(app.palette.accent, 16.0));
            ui.label(t("Ready — choose a source and press Start (Ctrl+R)"));
        });
        return;
    }

    ui.label(RichText::new(t("Before a conversation can be transcribed:")).strong());
    ui.add_space(6.0);
    for check in checks {
        ui.horizontal_wrapped(|ui| {
            let color = match check.status {
                readiness::Status::Met => app.palette.accent,
                readiness::Status::Unmet => app.palette.warning,
                readiness::Status::Waiting => app.palette.secondary,
            };
            match check.status {
                readiness::Status::Met => {
                    ui.add(Icon::StatusOk.image(color, 14.0));
                }
                readiness::Status::Unmet => {
                    ui.add(Icon::StatusWarn.image(color, 14.0));
                }
                // No third icon: a spinner says "still looking" better than any
                // glyph, and a warning triangle for a probe that has not
                // answered yet would be a problem the app invented.
                readiness::Status::Waiting => {
                    ui.add(egui::Spinner::new().size(14.0));
                }
            }
            ui.label(RichText::new(check.label).strong().color(app.palette.text));
            ui.label(RichText::new(&check.detail).color(color));
            if let Some(fix) = &check.fix {
                if ui.small_button(fix.button).clicked() {
                    ui.ctx().copy_text(fix.text.clone());
                }
            }
        });
        ui.add_space(4.0);
    }
}
