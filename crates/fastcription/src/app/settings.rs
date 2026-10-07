//! Source, engine/model, chunk tuning, mic track, voxtype service control,
//! and the disabled "translate to my language" toggle (decision D4:
//! `Segment::translation` is reserved for it; nothing produces it yet).

use crate::app::{App, ServiceStatus};
use crate::i18n::t;

pub struct State {
    pub engine: String,
    pub model: String,
    pub language: String,
    pub chunk_target_secs: f32,
    pub chunk_max_secs: f32,
}

impl Default for State {
    fn default() -> Self {
        Self {
            engine: "whisper".to_owned(),
            model: "base.en".to_owned(),
            language: "en".to_owned(),
            chunk_target_secs: 7.0,
            chunk_max_secs: 15.0,
        }
    }
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    ui.heading(t("Settings"));
    ui.separator();

    ui.label(t("Audio source"));
    crate::app::source_combo(app, ui, "settings-source");
    ui.checkbox(
        &mut app.mic_track,
        t("Capture my microphone as a second track"),
    );

    ui.add_space(8.0);
    ui.label(t("Engine"));
    ui.horizontal(|ui| {
        ui.label(t("Engine"));
        ui.text_edit_singleline(&mut app.settings.engine);
        ui.label(t("Model"));
        ui.text_edit_singleline(&mut app.settings.model);
        ui.label(t("Language"));
        ui.text_edit_singleline(&mut app.settings.language);
    });

    ui.add_space(8.0);
    ui.label(t("Chunking"));
    ui.add(
        egui::Slider::new(&mut app.settings.chunk_target_secs, 1.5..=10.0)
            .text(t("target seconds")),
    );
    ui.add(
        egui::Slider::new(&mut app.settings.chunk_max_secs, 7.0..=20.0).text(t(
            "max seconds (the segmenter grows into this under pressure)",
        )),
    );

    ui.add_space(8.0);
    ui.label(t("voxtype service"));
    ui.horizontal(|ui| {
        ui.label(service_label(app.voxtype_service));
        if ui.button(t("Start")).clicked() {
            app.set_service_running(true);
        }
        if ui.button(t("Stop")).clicked() {
            app.set_service_running(false);
        }
        if ui.button(t("Refresh")).clicked() {
            app.voxtype_service = crate::env::service_status();
        }
    });

    ui.add_space(8.0);
    let mut translate = false;
    ui.add_enabled_ui(false, |ui| {
        ui.checkbox(&mut translate, t("Translate to my language"))
            .on_disabled_hover_text(t(
                "Not implemented yet — Segment::translation is reserved for it (decision D4).",
            ));
    });
}

fn service_label(status: ServiceStatus) -> &'static str {
    match status {
        ServiceStatus::Unknown => "unknown",
        ServiceStatus::Running => "running",
        ServiceStatus::Stopped => "stopped",
    }
}
