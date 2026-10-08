//! Source and microphone choice, which voxtype engine and model to use, chunk
//! tuning, and control of the voxtype service.
//!
//! Engine and model are chosen from what voxtype reports as actually compiled
//! in and installed, rather than typed: a mistyped model name is only
//! discovered when the first chunk fails, which during a meeting is the worst
//! possible moment to find out.

use crate::app::{transcript, App, ServiceStatus};
use crate::i18n::t;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(default)]
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
    /// CPU threads for inference. Ignored when a server does the work.
    pub threads: u32,
    /// Send audio to a transcription server instead of running the model here.
    pub remote_enabled: bool,
    pub remote_endpoint: String,
    pub remote_model: String,
    /// Never persisted: egui's storage is a plain-text RON file in the user's
    /// data directory (`~/.local/share/fastcription/app.ron`), and this is a
    /// bearer token. It reaches voxtype through the per-session config, which
    /// is created 0600.
    #[serde(skip)]
    pub remote_api_key: String,
    pub remote_timeout_secs: u32,
    /// How large the transcript is drawn, in the live view, the history view
    /// and compact mode alike. Someone who reads a language better than they
    /// hear it is reading this across a room from a laptop, so it is a setting
    /// rather than a constant — and one with a slider in the Live pane and
    /// `Ctrl+=`/`Ctrl+-`/`Ctrl+0` on it, because the right size depends on
    /// where the laptop is sitting right now.
    pub transcript_pt: f32,
    /// The result of the last connection test, shown next to the button.
    ///
    /// Not persisted: "reached the server in 240 ms" said nothing about the
    /// network the app was restarted onto.
    #[serde(skip)]
    pub remote_probe: Option<Result<String, String>>,
    /// A connection test in flight on its own thread.
    #[serde(skip)]
    pub remote_probe_rx: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
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
            remote_enabled: false,
            remote_endpoint: String::new(),
            remote_model: "whisper-1".to_owned(),
            remote_api_key: String::new(),
            remote_timeout_secs: 30,
            transcript_pt: transcript::DEFAULT_PT,
            remote_probe: None,
            remote_probe_rx: None,
        }
    }
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    ui.heading(t("Settings"));
    ui.separator();

    ui.label(t("Audio source"));
    crate::app::chrome::source_combo(app, ui, "settings-source");

    ui.add_space(8.0);
    crate::app::chrome::mic_checkbox(app, ui, t("Capture my microphone as a second track"));
    ui.add_enabled_ui(app.mic_track && !app.source_locked(), |ui| {
        ui.horizontal(|ui| {
            ui.label(t("Microphone"));
            mic_combo(app, ui);
        });
    });

    ui.add_space(8.0);
    ui.label(t("Transcription"));
    ui.add_enabled_ui(!app.settings.remote_enabled, |ui| {
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
    });

    ui.add_space(8.0);
    ui.label(t("Transcription server"));
    ui.checkbox(
        &mut app.settings.remote_enabled,
        t("Run the model on another computer"),
    )
    .on_hover_text(t(
        "Sends each pass to an OpenAI-compatible transcription server. The \
         model stays loaded there, so a machine with a GPU can serve one \
         without, and no pass pays to load the model.",
    ));
    ui.add_enabled_ui(app.settings.remote_enabled, |ui| {
        ui.horizontal(|ui| {
            ui.label(t("Address"));
            ui.add(
                egui::TextEdit::singleline(&mut app.settings.remote_endpoint)
                    .desired_width(240.0)
                    .hint_text("http://desktop.lan:8080"),
            );
            ui.label(t("Model"));
            ui.add(
                egui::TextEdit::singleline(&mut app.settings.remote_model)
                    .desired_width(130.0)
                    .hint_text("whisper-1"),
            );
        });
        ui.horizontal(|ui| {
            ui.label(t("API key"));
            ui.add(
                egui::TextEdit::singleline(&mut app.settings.remote_api_key)
                    .desired_width(240.0)
                    .password(true)
                    .hint_text(t("optional")),
            )
            .on_hover_text(t(
                "Kept for this session only. It is written to fastcription's own \
                 voxtype config, which is created readable by you alone, and never \
                 to the settings file that remembers everything else here.",
            ));
            ui.add(
                egui::Slider::new(&mut app.settings.remote_timeout_secs, 5..=120)
                    .text(t("timeout s")),
            );
        });
        ui.horizontal(|ui| {
            let testing = app.settings.remote_probe_rx.is_some();
            if ui
                .add_enabled(!testing, egui::Button::new(t("Test connection")))
                .clicked()
            {
                app.probe_transcription_server();
            }
            if testing {
                // The only interesting failure is an address nothing answers
                // at, which takes the configured timeout to discover. The
                // spinner is what says the window is still alive.
                ui.add(egui::Spinner::new().size(14.0));
                ui.label(
                    egui::RichText::new(t("asking the server…"))
                        .small()
                        .color(app.palette.secondary),
                );
                return;
            }
            match &app.settings.remote_probe {
                Some(Ok(text)) => ui.colored_label(app.palette.accent, text.clone()),
                Some(Err(text)) => ui.colored_label(app.palette.danger, text.clone()),
                None => ui.label(
                    egui::RichText::new(t("not tested"))
                        .small()
                        .color(app.palette.secondary),
                ),
            };
        });
        if app.settings.remote_endpoint.starts_with("http://")
            && !app.settings.remote_endpoint.contains("localhost")
            && !app.settings.remote_endpoint.contains("127.0.0.1")
        {
            ui.label(
                egui::RichText::new(t(
                    "This address is not encrypted, so the audio crosses the \
                     network in the clear. Fine on a network you trust; use a \
                     tunnel otherwise.",
                ))
                .small()
                .color(app.palette.warning),
            );
        }
    });
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

    if ui
        .small_button(t("Use voxtype's defaults"))
        .on_hover_text(t(
            "Reads the engine, model and language from ~/.config/voxtype/config.toml \
             again. Saved settings otherwise win over that file on every launch.",
        ))
        .clicked()
    {
        app.reset_engine_to_voxtype();
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
    ui.label(t("Transcript"));
    ui.add(
        egui::Slider::new(&mut app.settings.transcript_pt, transcript::PT_RANGE)
            .step_by(1.0)
            .text(t("text size")),
    )
    .on_hover_text(t(
        "How large the words are drawn in the live view, in a conversation's \
         history and in compact mode. The Live pane has the same slider, and \
         Ctrl+= / Ctrl+- / Ctrl+0 reach it from anywhere.",
    ));

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
    ui.horizontal(|ui| {
        let running = app.import_running();
        if ui
            .add_enabled(
                running.is_none(),
                egui::Button::new(t("Import past voxtype meetings")),
            )
            .on_hover_text(t(
                "Copies meetings recorded by voxtype's own meeting mode into this \
                 library. Already imported meetings are skipped.",
            ))
            .clicked()
        {
            app.import_voxtype_meetings();
        }
        if let Some(added) = running {
            ui.add(egui::Spinner::new().size(14.0));
            ui.label(
                egui::RichText::new(format!("{added} {}", t("imported so far")))
                    .small()
                    .color(app.palette.secondary),
            );
        }
    });
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
