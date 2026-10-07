//! The always-on-top caption overlay (decision D3): a second, undecorated
//! viewport showing the last few committed lines in a large font, so it can
//! float over a meeting window independent of the main one.
//!
//! Wayland gives winit no layer-shell, so `with_always_on_top` here is a
//! request a compositor is free to ignore; `docs/OVERLAY.md` has the
//! Hyprland window rule that forces it, and the sway/river equivalent.

use std::sync::{Arc, Mutex};

use egui::{Color32, RichText, ViewportBuilder, ViewportClass, ViewportId};

const MAX_LINES: usize = 5;

#[derive(Clone)]
pub struct Line {
    pub speaker: String,
    pub text: String,
}

#[derive(Default)]
pub struct State {
    pub lines: Vec<Line>,
}

/// Shared with the overlay's own viewport closure, which egui may call on a
/// different schedule than the main window's.
pub type Shared = Arc<Mutex<State>>;

/// Refreshes the overlay's snapshot from the committed transcript. Called
/// whenever a segment commits; cheap enough to just clone the last few lines.
pub fn sync(shared: &Shared, segments: &[fc_core::Segment]) {
    let Ok(mut state) = shared.lock() else {
        return;
    };
    state.lines = segments
        .iter()
        .rev()
        .take(MAX_LINES)
        .map(|segment| Line {
            speaker: segment.speaker_label().to_owned(),
            text: segment.text.clone(),
        })
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
}

/// Declares the overlay viewport for this pass. Must be called every frame
/// the overlay should stay open; dropping the call closes the window.
pub fn show(ctx: &egui::Context, shared: &Shared) {
    let shared = Arc::clone(shared);
    ctx.show_viewport_deferred(
        ViewportId::from_hash_of("fastcription-overlay"),
        ViewportBuilder::default()
            .with_title("fastcription captions")
            .with_inner_size([760.0, 170.0])
            .with_min_inner_size([320.0, 90.0])
            .with_decorations(false)
            .with_always_on_top()
            .with_transparent(true),
        move |ui, class| {
            if class == ViewportClass::EmbeddedWindow {
                egui::CentralPanel::default().show(ui, |ui| {
                    ui.label(
                        "This egui backend has no second window; the overlay is embedded here.",
                    );
                });
                return;
            }
            let lines = shared
                .lock()
                .map(|state| state.lines.clone())
                .unwrap_or_default();
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::default()
                        .fill(Color32::from_black_alpha(215))
                        .inner_margin(12.0),
                )
                .show(ui, |ui| {
                    if lines.is_empty() {
                        ui.label(
                            RichText::new("Waiting for speech…")
                                .color(Color32::GRAY)
                                .size(22.0),
                        );
                    }
                    for line in &lines {
                        ui.label(
                            RichText::new(format!("{}: {}", line.speaker, line.text))
                                .color(Color32::WHITE)
                                .size(26.0),
                        );
                    }
                });
        },
    );
}
