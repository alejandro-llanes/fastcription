//! Source and microphone choice, which voxtype engine and model to use, chunk
//! tuning, and control of the voxtype service.
//!
//! Engine and model are chosen from what voxtype reports as actually compiled
//! in and installed, rather than typed: a mistyped model name is only
//! discovered when the first chunk fails, which during a meeting is the worst
//! possible moment to find out.

use crate::app::{App, ServiceStatus};
use crate::i18n::t;

pub struct State {
    /// Engine, model and language as they will be passed to voxtype. Seeded
    /// from voxtype's own configuration at startup, so leaving them alone
    /// reproduces what voxtype would have done by itself.
    pub engine: String,
    pub model: String,
    pub language: String,
    /// How often the current utterance is re-transcribed. Lower shows words
    /// sooner and costs more CPU.
    pub refresh_secs: f32,
    /// How long an utterance may run before it is finalised without a pause.
    /// Kept under voxtype's 22.5 second context-optimisation threshold.
    pub max_utterance_secs: f32,
    /// voxtype's `context_window_optimization`: two to nearly three times
    /// faster under 22.5 seconds, which is what makes realtime possible.
    pub fast_mode: bool,
    /// CPU threads for inference.
    pub threads: u32,
}

impl Default for State {
    fn default() -> Self {
        Self {
            engine: "whisper".to_owned(),
            model: "base.en".to_owned(),
            language: "en".to_owned(),
            refresh_secs: 1.0,
            max_utterance_secs: 20.0,
            fast_mode: true,
            threads: crate::env::default_threads(),
        }
    }
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    ui.heading(t("Settings"));
    ui.separator();

    ui.label(t("Audio source"));
    crate::app::source_combo(app, ui, "settings-source");

    ui.add_space(8.0);
    ui.checkbox(
        &mut app.mic_track,
        t("Capture my microphone as a second track"),
    );
    ui.add_enabled_ui(app.mic_track, |ui| {
        ui.horizontal(|ui| {
            ui.label(t("Microphone"));
            mic_combo(app, ui);
        });
    });

    ui.add_space(8.0);
    ui.label(t("Transcription"));
    ui.horizontal(|ui| {
        ui.label(t("Engine"));
        combo(ui, "engine", &mut app.settings.engine, &app.engines);
        ui.label(t("Model"));
        combo(ui, "model", &mut app.settings.model, &app.models);
        ui.label(t("Language"));
        ui.add(
            egui::TextEdit::singleline(&mut app.settings.language)
                .desired_width(60.0)
                .hint_text("en"),
        )
        .on_hover_text(t(
            "A language code such as en or es, a comma-separated list, or auto.",
        ));
    });
    ui.checkbox(&mut app.settings.fast_mode, t("Fast mode"))
        .on_hover_text(t(
            "Uses voxtype's context-window optimisation: two to nearly three \
             times faster for the short passes realtime needs. Turn it off if \
             you see words repeating.",
        ));
    ui.add(egui::Slider::new(&mut app.settings.threads, 1..=32).text(t("inference threads")))
        .on_hover_text(t(
            "whisper.cpp stops getting faster past about eight threads for the \
             small English models.",
        ));
    if let Some(backend) = app.engine.backend.as_deref() {
        ui.label(
            egui::RichText::new(format!("{} {backend}", t("Acceleration:")))
                .small()
                .color(app.palette.secondary),
        );
    }
    if app.engines.is_empty() && app.models.is_empty() {
        ui.label(
            egui::RichText::new(t(
                "voxtype did not report its engines or models; the fields above are free text.",
            ))
            .small()
            .color(app.palette.secondary),
        );
    }

    ui.add_space(8.0);
    ui.label(t("Responsiveness"));
    ui.add(
        egui::Slider::new(&mut app.settings.refresh_secs, 0.5..=3.0)
            .text(t("seconds between passes")),
    );
    ui.add(
        egui::Slider::new(&mut app.settings.max_utterance_secs, 8.0..=22.0)
            .text(t("longest utterance in seconds")),
    );
    ui.label(
        egui::RichText::new(t(
            "The sentence being spoken is re-transcribed this often, and words \
             appear once two passes agree on them. Shorter means words sooner \
             and more CPU. Changes apply to the next conversation.",
        ))
        .small()
        .color(app.palette.secondary),
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
    });
    ui.label(
        egui::RichText::new(t(
            "fastcription transcribes without the daemon; the service is what \
             provides voxtype's own push-to-talk dictation.",
        ))
        .small()
        .color(app.palette.secondary),
    );


    ui.add_space(8.0);
    ui.label(t("Import"));
    if ui
        .button(t("Import past voxtype meetings"))
        .on_hover_text(t(
            "Copies meetings recorded by voxtype's own meeting mode into this \
             library. Already imported meetings are skipped.",
        ))
        .clicked()
    {
        app.import_voxtype_meetings();
    }
}

/// An editable choice: a dropdown of what voxtype reports, which still accepts
/// a value that is not in the list, because `voxtype info` can fail while
/// transcription works perfectly well.
fn combo(ui: &mut egui::Ui, id_salt: &str, current: &mut String, options: &[String]) {
    if options.is_empty() {
        ui.add(
            egui::TextEdit::singleline(current)
                .id_salt(id_salt)
                .desired_width(110.0),
        );
        return;
    }
    egui::ComboBox::from_id_salt(id_salt)
        .selected_text(current.clone())
        .width(130.0)
        .show_ui(ui, |ui| {
            for option in options {
                ui.selectable_value(current, option.clone(), option);
            }
        });
}

fn mic_combo(app: &mut App, ui: &mut egui::Ui) {
    let current = app.mic_source.label();
    egui::ComboBox::from_id_salt("settings-mic")
        .selected_text(current)
        .show_ui(ui, |ui| {
            let default = crate::env::default_microphone();
            let label = default.label();
            ui.selectable_value(&mut app.mic_source, default, label);
            for source in app.microphones() {
                let label = source.label();
                ui.selectable_value(&mut app.mic_source, source, label);
            }
        });
}

fn service_label(status: ServiceStatus) -> &'static str {
    match status {
        ServiceStatus::Unknown => "not installed",
        ServiceStatus::Running => "running",
        ServiceStatus::Stopped => "stopped",
    }
}
