//! Everything configurable, in five categories.
//!
//! This pane used to be one column: a dozen bold labels with their controls
//! under them, scrolling off the bottom of the window. Every setting was
//! equally prominent, which meant none of them were, and two unrelated fields
//! both called "Model" sat four lines apart — the engine's and the server's.
//! The grouping below is the fix, and the categories are chosen by *when* a
//! setting is touched rather than by what it configures: **Audio** and
//! **Transcription** are set up once, **Server** only if there is one,
//! **Appearance** whenever the room or the mood changes, and **System** is the
//! things that reach outside the app.
//!
//! Engine and model are chosen from what voxtype reports as actually compiled
//! in and installed, rather than typed: a mistyped model name is only
//! discovered when the first chunk fails, which during a meeting is the worst
//! possible moment to find out.

use egui::{Color32, Margin, RichText, Stroke, Ui};

use crate::app::{transcript, App, ServiceStatus};
use crate::i18n::t;
use crate::theme::{Palette, ThemeChoice};
use crate::visualizer;

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
    /// Where the palette comes from. Applies to the full window and to compact
    /// mode alike; they are the same window.
    pub theme: ThemeChoice,
    /// Which shape the audio visualiser draws.
    pub visualizer: visualizer::Style,
    /// Whether the full window shows the visualiser as well as compact mode.
    ///
    /// Separate from the style because the two places are used differently:
    /// compact mode is a strip where the visualiser is most of what there is
    /// to see, and the main window is somewhere a reader is trying to follow
    /// a transcript. Someone can reasonably want it in one and not the other.
    pub visualizer_in_main: bool,
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
            theme: ThemeChoice::default(),
            visualizer: visualizer::Style::default(),
            visualizer_in_main: true,
            remote_probe: None,
            remote_probe_rx: None,
        }
    }
}

/// Which group of settings is showing.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Tab {
    #[default]
    Audio,
    Transcription,
    Server,
    Appearance,
    System,
}

impl Tab {
    pub const ALL: [Tab; 5] = [
        Tab::Audio,
        Tab::Transcription,
        Tab::Server,
        Tab::Appearance,
        Tab::System,
    ];

    fn label(self) -> &'static str {
        match self {
            Tab::Audio => t("Audio"),
            Tab::Transcription => t("Transcription"),
            Tab::Server => t("Server"),
            Tab::Appearance => t("Appearance"),
            Tab::System => t("System"),
        }
    }
}

/// How wide the settings column is allowed to get.
///
/// The pane is as wide as the window, which on a desktop is most of a
/// 1920-point screen. A card stretched that far puts its checkbox at one end
/// of the room and its explanation at the other, and a line of prose that long
/// is genuinely harder to read — the usual advice is to stop somewhere around
/// 80 characters, which this is.
const CONTENT_WIDTH: f32 = 720.0;

pub fn show(app: &mut App, ui: &mut Ui) {
    crate::ui::label(ui, &app.palette.clone(), t("settings"));
    ui.add_space(8.0);
    tab_row(app, ui);
    ui.add_space(12.0);

    egui::ScrollArea::vertical()
        .id_salt("settings-scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_max_width(CONTENT_WIDTH.min(ui.available_width()));
            body(app, ui);
        });
}

fn body(app: &mut App, ui: &mut Ui) {
    match app.settings_tab {
        Tab::Audio => audio(app, ui),
        Tab::Transcription => transcription(app, ui),
        Tab::Server => server(app, ui),
        Tab::Appearance => appearance(app, ui),
        Tab::System => system(app, ui),
    }
}

/// The category switcher: one pill row, the active one filled with the accent.
///
/// `horizontal_wrapped` rather than `horizontal`, because the window's minimum
/// width is 760 and five labels plus the sidebar do not always fit on one line
/// — and a tab that is off the edge of its own switcher is a tab nobody can
/// reach.
fn tab_row(app: &mut App, ui: &mut Ui) {
    let palette = app.palette.clone();
    egui::Frame::default()
        .fill(palette.surface)
        .corner_radius(10)
        .inner_margin(Margin::same(4))
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                for tab in Tab::ALL {
                    let selected = app.settings_tab == tab;
                    let label = RichText::new(tab.label()).color(if selected {
                        palette.on_accent
                    } else {
                        palette.secondary
                    });
                    let button = egui::Button::new(label)
                        .fill(if selected {
                            palette.accent
                        } else {
                            Color32::TRANSPARENT
                        })
                        .stroke(Stroke::NONE)
                        .corner_radius(8)
                        .min_size(egui::vec2(0.0, 26.0));
                    if ui.add(button).clicked() {
                        app.settings_tab = tab;
                    }
                }
            });
        });
}

/// One group of settings, in a panel of its own.
///
/// The boundary the old bold labels were only implying. Cards also give the
/// pane somewhere to put a group that is currently irrelevant — the server
/// block greys out as a block rather than as eight separate disabled widgets.
fn card<R>(ui: &mut Ui, palette: &Palette, title: &str, body: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::default()
        .fill(palette.surface)
        .stroke(Stroke::new(1.0, palette.outline))
        .corner_radius(10)
        .inner_margin(Margin::same(12))
        .outer_margin(Margin {
            bottom: 10,
            ..Margin::ZERO
        })
        .show(ui, |ui| {
            // Without this a card is only as wide as its widest control, so a
            // column of them comes out ragged — each one a different width,
            // none of them lining up with the next.
            ui.set_width(ui.available_width());
            crate::ui::inset_controls(ui, palette);
            ui.label(RichText::new(title).strong().color(palette.text));
            ui.add_space(8.0);
            body(ui)
        })
        .inner
}

/// The small grey sentence under a control that explains it.
fn hint(ui: &mut Ui, palette: &Palette, text: &str) {
    ui.add_space(4.0);
    ui.label(RichText::new(text).small().color(palette.secondary));
}

// ---------------------------------------------------------------- Audio

fn audio(app: &mut App, ui: &mut Ui) {
    let palette = app.palette.clone();

    card(ui, &palette, t("What to transcribe"), |ui| {
        crate::app::chrome::source_combo(app, ui, "settings-source");
        hint(
            ui,
            &palette,
            t(
                "A monitor records what a meeting is playing through your speakers. \
               A capture device records a microphone or line input.",
            ),
        );
    });

    card(ui, &palette, t("Your own voice"), |ui| {
        crate::app::chrome::mic_checkbox(app, ui, t("Capture my microphone as a second track"));
        ui.add_enabled_ui(app.mic_track && !app.source_locked(), |ui| {
            ui.horizontal(|ui| {
                ui.label(t("Microphone"));
                mic_combo(app, ui);
            });
        });
        hint(
            ui,
            &palette,
            t(
                "Recorded and transcribed alongside the meeting, so the transcript \
               has both halves of the conversation.",
            ),
        );
    });
}

// -------------------------------------------------------- Transcription

fn transcription(app: &mut App, ui: &mut Ui) {
    let palette = app.palette.clone();

    if app.settings.remote_enabled {
        card(ui, &palette, t("A server is doing the work"), |ui| {
            ui.label(
                RichText::new(t(
                    "These settings are for the model running on this computer, and \
                     a transcription server is configured. Turn that off under \
                     Server to use them.",
                ))
                .color(palette.secondary),
            );
        });
    }

    ui.add_enabled_ui(!app.settings.remote_enabled, |ui| {
        card(ui, &palette, t("Model"), |ui| {
            ui.horizontal_wrapped(|ui| {
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
            if app.engines.is_empty() && app.models.is_empty() {
                hint(
                    ui,
                    &palette,
                    t("voxtype did not report its engines or models; these are free text."),
                );
            }
            ui.add_space(6.0);
            if ui
                .small_button(t("Use voxtype's defaults"))
                .on_hover_text(t("Reads the engine, model and language from \
                     ~/.config/voxtype/config.toml again. Saved settings otherwise \
                     win over that file on every launch."))
                .clicked()
            {
                app.reset_engine_to_voxtype();
            }
        });

        card(ui, &palette, t("Speed"), |ui| {
            ui.checkbox(&mut app.settings.fast_mode, t("Fast mode"))
                .on_hover_text(t(
                    "Uses voxtype's context-window optimisation: two to nearly three \
                     times faster for the short passes realtime needs. Turn it off if \
                     you see words repeating.",
                ));
            ui.add(
                egui::Slider::new(&mut app.settings.threads, 1..=32).text(t("inference threads")),
            )
            .on_hover_text(t(
                "whisper.cpp stops getting faster past about eight threads for the \
                 small English models.",
            ));
            accel_line(app, ui, &palette);
        });
    });

    card(ui, &palette, t("Responsiveness"), |ui| {
        ui.add(
            egui::Slider::new(&mut app.settings.refresh_secs, 0.5..=3.0)
                .text(t("seconds between passes")),
        );
        ui.add(
            egui::Slider::new(&mut app.settings.max_utterance_secs, 8.0..=22.0)
                .text(t("longest utterance in seconds")),
        );
        hint(
            ui,
            &palette,
            t(
                "The sentence being spoken is re-transcribed this often, and words \
               appear once two passes agree on them. Shorter means words sooner \
               and more CPU. Changes apply to the next conversation.",
            ),
        );
    });
}

/// What is actually doing the arithmetic, and what to do if it is the CPU.
///
/// The readiness checklist says this too, and says it louder; here it is
/// beside the thread slider, which is the control someone reaches for when
/// transcription is slow and the real answer is that the GPU is not in use.
fn accel_line(app: &App, ui: &mut Ui, palette: &Palette) {
    let Some(backend) = app.engine.backend.as_deref() else {
        return;
    };
    let cpu = backend.to_lowercase().contains("cpu");
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(format!("{} {backend}", t("Acceleration:")))
                .small()
                .color(if cpu {
                    palette.warning
                } else {
                    palette.secondary
                }),
        );
        if cpu
            && ui
                .small_button(t("Copy command"))
                .on_hover_text(t(
                    "voxtype ships prebuilt GPU variants; this asks it to switch to one.",
                ))
                .clicked()
        {
            ui.ctx().copy_text("voxtype setup gpu --enable".to_owned());
        }
    });
}

// --------------------------------------------------------------- Server

fn server(app: &mut App, ui: &mut Ui) {
    let palette = app.palette.clone();

    card(ui, &palette, t("Transcription server"), |ui| {
        ui.checkbox(
            &mut app.settings.remote_enabled,
            t("Run the model on another computer"),
        )
        .on_hover_text(t(
            "Sends each pass to an OpenAI-compatible transcription server. The \
             model stays loaded there, so a machine with a GPU can serve one \
             without, and no pass pays to load the model.",
        ));
        hint(
            ui,
            &palette,
            t(
                "docs/SERVER.md walks through setting one up, including which model \
               to run and how to open the port.",
            ),
        );
    });

    ui.add_enabled_ui(app.settings.remote_enabled, |ui| {
        card(ui, &palette, t("Where it is"), |ui| {
            ui.horizontal_wrapped(|ui| {
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
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
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
            if app.settings.remote_endpoint.starts_with("http://")
                && !app.settings.remote_endpoint.contains("localhost")
                && !app.settings.remote_endpoint.contains("127.0.0.1")
            {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(t(
                        "This address is not encrypted, so the audio crosses the \
                         network in the clear. Fine on a network you trust; use a \
                         tunnel otherwise.",
                    ))
                    .small()
                    .color(palette.warning),
                );
            }
        });

        card(ui, &palette, t("Does it answer?"), |ui| {
            ui.horizontal_wrapped(|ui| {
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
                        RichText::new(t("asking the server…"))
                            .small()
                            .color(palette.secondary),
                    );
                    return;
                }
                match &app.settings.remote_probe {
                    Some(Ok(text)) => ui.colored_label(palette.accent, text.clone()),
                    Some(Err(text)) => ui.colored_label(palette.danger, text.clone()),
                    None => ui.label(
                        RichText::new(t("not tested"))
                            .small()
                            .color(palette.secondary),
                    ),
                };
            });
        });
    });
}

// ----------------------------------------------------------- Appearance

fn appearance(app: &mut App, ui: &mut Ui) {
    let palette = app.palette.clone();

    card(ui, &palette, t("Theme"), |ui| {
        let current = app.settings.theme.label();
        egui::ComboBox::from_id_salt("settings-theme")
            .selected_text(current)
            .width(220.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut app.settings.theme,
                    ThemeChoice::System,
                    ThemeChoice::System.label(),
                );
                ui.selectable_value(
                    &mut app.settings.theme,
                    ThemeChoice::Dark,
                    ThemeChoice::Dark.label(),
                );
                ui.selectable_value(
                    &mut app.settings.theme,
                    ThemeChoice::Light,
                    ThemeChoice::Light.label(),
                );
                // Collected first: the picker borrows `app.settings` mutably
                // while the catalog is borrowed from `app`.
                let named: Vec<String> = app
                    .theme_catalog
                    .picker_themes()
                    .map(|theme| theme.filename.clone())
                    .collect();
                if !named.is_empty() {
                    ui.separator();
                }
                for filename in named {
                    let choice = ThemeChoice::Named(filename);
                    let label = choice.label();
                    ui.selectable_value(&mut app.settings.theme, choice, label);
                }
            });
        hint(
            ui,
            &palette,
            t(
                "The full window and compact mode share one theme — they are the \
               same window. Following the desktop keeps fastcription in step with \
               Omarchy as you switch themes.",
            ),
        );
    });

    card(ui, &palette, t("Audio visualiser"), |ui| {
        ui.horizontal_wrapped(|ui| {
            for style in visualizer::Style::ALL {
                let selected = app.settings.visualizer == style;
                let label = RichText::new(style.label()).color(if selected {
                    palette.on_accent
                } else {
                    palette.text
                });
                let button = egui::Button::new(label)
                    .fill(if selected {
                        palette.accent
                    } else {
                        palette.surface_hover
                    })
                    .corner_radius(8)
                    .min_size(egui::vec2(0.0, 26.0));
                if ui.add(button).clicked() {
                    app.settings.visualizer = style;
                }
            }
        });

        ui.add_space(10.0);
        // A live preview, because the names alone do not tell anyone what
        // "Ribbons" looks like. With nothing being recorded there is no
        // spectrum to draw, so this one is given a synthetic one — the only
        // place in the app that draws audio nobody made, which is why it says
        // so underneath.
        let phase = ui.input(|i| i.time) as f32;
        app.preview_visualizer.feed(visualizer::demo_bands(phase));
        let drawn = app
            .preview_visualizer
            .show(ui, &palette, app.settings.visualizer, egui::vec2(0.0, 64.0))
            .is_some();
        if drawn {
            hint(ui, &palette, t("Preview — not live audio."));
        } else {
            ui.label(
                RichText::new(t("The visualiser is off."))
                    .small()
                    .color(palette.secondary),
            );
        }

        ui.add_space(8.0);
        ui.add_enabled_ui(app.settings.visualizer.visible(), |ui| {
            ui.checkbox(
                &mut app.settings.visualizer_in_main,
                t("Show it in the full window too"),
            )
            .on_hover_text(t(
                "Compact mode always shows it. The full window is somewhere you are \
                 reading, so this is yours to decide.",
            ));
        });
    });

    card(ui, &palette, t("Transcript"), |ui| {
        ui.add(
            egui::Slider::new(&mut app.settings.transcript_pt, transcript::PT_RANGE)
                .step_by(1.0)
                .text(t("text size")),
        )
        .on_hover_text(t(
            "How large the words are drawn in the live view, in a conversation's \
             history and in compact mode.",
        ));
        hint(
            ui,
            &palette,
            t(
                "The Live pane has the same slider, and Ctrl+= / Ctrl+- / Ctrl+0 \
               reach it from anywhere — including compact mode.",
            ),
        );
    });
}

// --------------------------------------------------------------- System

fn system(app: &mut App, ui: &mut Ui) {
    let palette = app.palette.clone();

    card(ui, &palette, t("voxtype service"), |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(service_label(app.voxtype_service));
            if ui.button(t("Start")).clicked() {
                app.set_service_running(true);
            }
            if ui.button(t("Stop")).clicked() {
                app.set_service_running(false);
            }
        });
        hint(
            ui,
            &palette,
            t(
                "fastcription transcribes without the daemon; the service is what \
               provides voxtype's own push-to-talk dictation.",
            ),
        );
    });

    card(ui, &palette, t("Import"), |ui| {
        ui.horizontal_wrapped(|ui| {
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
                    RichText::new(format!("{added} {}", t("imported so far")))
                        .small()
                        .color(palette.secondary),
                );
            }
        });
    });

    card(ui, &palette, t("Library"), |ui| {
        ui.label(
            RichText::new(app.library_path.display().to_string())
                .small()
                .color(palette.secondary),
        );
        ui.add_space(4.0);
        if ui.small_button(t("Copy path")).clicked() {
            ui.ctx().copy_text(app.library_path.display().to_string());
        }
    });
}

/// An editable choice: a dropdown of what voxtype reports, which still accepts
/// a value that is not in the list, because `voxtype info` can fail while
/// transcription works perfectly well.
fn combo(ui: &mut Ui, id_salt: &str, current: &mut String, options: &[String]) {
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

fn mic_combo(app: &mut App, ui: &mut Ui) {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `ALL` is what the tab row draws, so a tab missing from it is a tab
    /// nobody can reach and a group of settings nobody can change.
    #[test]
    fn every_tab_is_listed_once_and_has_a_label() {
        let mut seen = Tab::ALL.to_vec();
        seen.sort_by_key(|tab| format!("{tab:?}"));
        seen.dedup();
        assert_eq!(seen.len(), Tab::ALL.len());
        for tab in Tab::ALL {
            assert!(!tab.label().is_empty(), "{tab:?}");
        }
    }

    /// The pane opens on whichever tab this is, and `Audio` is the one with
    /// the setting that has to be right before anything works at all.
    #[test]
    fn the_first_tab_is_the_one_that_matters_first() {
        assert_eq!(Tab::default(), Tab::Audio);
        assert_eq!(Tab::ALL[0], Tab::Audio);
    }

    /// Defaults have to keep the app behaving as it did for someone who never
    /// opens this pane: following the desktop, and showing the visualiser.
    #[test]
    fn the_new_appearance_defaults_change_nothing_for_an_existing_user() {
        let state = State::default();
        assert_eq!(state.theme, ThemeChoice::System);
        assert!(state.visualizer.visible());
        assert!(state.visualizer_in_main);
    }
}
