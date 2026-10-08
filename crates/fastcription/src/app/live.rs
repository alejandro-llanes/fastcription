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
    let palette = app.palette.clone();
    crate::ui::pane(ui, &palette, |ui| {
        header(app, ui, &palette);
        ui.add_space(10.0);
        body(app, ui);
    });
}

/// The panel's own title row: what this is, how big to draw it, and the two
/// ways out of it.
fn header(app: &mut App, ui: &mut egui::Ui, palette: &crate::theme::Palette) {
    // Wrapped, because at the window's minimum width the label, the size
    // slider and the two copy buttons do not fit on one line.
    ui.horizontal_wrapped(|ui| {
        crate::ui::label(ui, palette, t("live transcript"));
        ui.add_space(10.0);
        // An explicit width: in a wrapped row the slider otherwise takes
        // whatever is left, which on a wide window is most of the header.
        ui.spacing_mut().slider_width = 110.0;
        ui.add(
            egui::Slider::new(&mut app.settings.transcript_pt, transcript::PT_RANGE)
                .step_by(1.0)
                .fixed_decimals(0)
                .suffix(" pt"),
        )
        .on_hover_text(t(
            "How large the transcript is drawn, here and in compact mode. \
             Ctrl+= and Ctrl+- do the same, and Ctrl+0 goes back to 22.",
        ));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let conversation = app
                .live_conversation
                .and_then(|id| app.conversations.iter().find(|c| c.id == id));
            transcript::copy_buttons(ui, conversation, &app.segments);
        });
    });
}

fn body(app: &mut App, ui: &mut egui::Ui) {
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
        // Ready is not the same as fast. An advisory is no reason to withhold
        // the line above or to hold the whole checklist open, but this is the
        // only place it is written down, and a reader who starts a meeting not
        // knowing the captions cannot keep up has been failed quietly.
        for check in checks
            .iter()
            .filter(|check| check.status == readiness::Status::Advisory)
        {
            ui.add_space(6.0);
            row(app, ui, check);
        }
        return;
    }

    ui.label(RichText::new(t("Before a conversation can be transcribed:")).strong());
    ui.add_space(6.0);
    for check in &checks {
        row(app, ui, check);
        ui.add_space(4.0);
    }
}

/// One line of the checklist: how it stands, what it is, what was found, and
/// the remedy on a button.
fn row(app: &App, ui: &mut egui::Ui, check: &readiness::Check) {
    use crate::icons::Icon;

    ui.horizontal_wrapped(|ui| {
        let color = match check.status {
            readiness::Status::Met => app.palette.accent,
            // An advisory is drawn exactly like an unmet requirement. It is
            // not one — recording works — but the thing it warns about makes
            // the application useless for its purpose, so it does not get to
            // look like a footnote.
            readiness::Status::Unmet | readiness::Status::Advisory => app.palette.warning,
            readiness::Status::Waiting => app.palette.secondary,
        };
        match check.status {
            readiness::Status::Met => {
                ui.add(Icon::StatusOk.image(color, 14.0));
            }
            readiness::Status::Unmet | readiness::Status::Advisory => {
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
}
