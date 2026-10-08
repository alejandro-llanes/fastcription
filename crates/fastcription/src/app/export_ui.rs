//! The export format picker. ARCHITECTURE.md §6 ties fastcription's export
//! formats to voxtype's own (`txt`, `md`, `json`, `srt`, `vtt`) so a
//! transcript from either is interchangeable downstream; `fc-export` owns
//! writing them; this picker chooses a format and the options that mirror
//! voxtype's own export flags.

use crate::app::App;
use crate::i18n::t;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Text,
    Markdown,
    Json,
    Srt,
    Vtt,
}

impl Format {
    const ALL: [Self; 5] = [Self::Text, Self::Markdown, Self::Json, Self::Srt, Self::Vtt];

    fn to_export(self) -> fc_export::ExportFormat {
        match self {
            Self::Text => fc_export::ExportFormat::Text,
            Self::Markdown => fc_export::ExportFormat::Markdown,
            Self::Json => fc_export::ExportFormat::Json,
            Self::Srt => fc_export::ExportFormat::Srt,
            Self::Vtt => fc_export::ExportFormat::Vtt,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Text => "Text (.txt)",
            Self::Markdown => "Markdown (.md)",
            Self::Json => "JSON (.json)",
            Self::Srt => "SubRip (.srt)",
            Self::Vtt => "WebVTT (.vtt)",
        }
    }
}

pub struct State {
    pub format: Format,
    pub open: bool,
    /// Mirrors `voxtype meeting export`'s flags, so the same transcript can be
    /// produced from either tool.
    pub timestamps: bool,
    pub speakers: bool,
    pub metadata: bool,
    /// Where the file goes, editable. There is no file dialog: a portal
    /// dependency is not worth it to pick one directory, but a path the user
    /// cannot see or change is worse than no dialog at all.
    pub destination: String,
    /// A destination that already holds a file, waiting for the second click
    /// that says to replace it. Cleared when the path changes, so confirming
    /// one path cannot authorise overwriting another.
    pub confirming: Option<std::path::PathBuf>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            format: Format::Text,
            open: false,
            timestamps: true,
            speakers: true,
            metadata: true,
            destination: String::new(),
            confirming: None,
        }
    }
}

/// The export button for the conversation currently open in the history view.
pub fn button(app: &mut App, ui: &mut egui::Ui, conversation: &fc_core::Conversation) {
    let clicked = ui
        .horizontal(|ui| {
            ui.add(crate::icons::Icon::Export.image(app.palette.text, 14.0));
            ui.button(t("Export")).clicked()
        })
        .inner;
    if clicked {
        app.export.open = !app.export.open;
        app.export.confirming = None;
        if app.export.open {
            app.export.destination = app
                .default_export_path(conversation, app.export.format.to_export())
                .display()
                .to_string();
        }
    }
    if app.export.open {
        egui::Window::new(t("Export conversation"))
            .collapsible(false)
            .resizable(false)
            .show(ui.ctx(), |ui| {
                let before = app.export.format;
                for format in Format::ALL {
                    ui.radio_value(&mut app.export.format, format, format.label());
                }
                if app.export.format != before {
                    // Keep the suggested filename's extension honest when the
                    // format changes, unless the user has typed their own path.
                    let suggested = app
                        .default_export_path(conversation, before.to_export())
                        .display()
                        .to_string();
                    if app.export.destination == suggested {
                        app.export.destination = app
                            .default_export_path(conversation, app.export.format.to_export())
                            .display()
                            .to_string();
                    }
                }
                ui.separator();
                ui.label(t("Save to"));
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut app.export.destination)
                            .desired_width(380.0),
                    )
                    .changed()
                {
                    // A confirmation belongs to the path it was given for.
                    app.export.confirming = None;
                }
                ui.separator();
                ui.checkbox(&mut app.export.timestamps, t("Timestamps"));
                ui.checkbox(&mut app.export.speakers, t("Speaker labels"));
                ui.checkbox(&mut app.export.metadata, t("Metadata header"));
                ui.separator();
                let destination =
                    std::path::PathBuf::from(shellexpand_home(&app.export.destination));
                let replacing = app.export.confirming.as_deref() == Some(destination.as_path());
                if replacing {
                    ui.label(
                        egui::RichText::new(format!(
                            "{} {}",
                            t("A file is already there:"),
                            destination.display()
                        ))
                        .small()
                        .color(app.palette.warning),
                    );
                }
                ui.horizontal(|ui| {
                    let label = if replacing {
                        t("Replace?")
                    } else {
                        t("Export")
                    };
                    if ui.button(label).clicked() {
                        let options = fc_export::ExportOptions {
                            timestamps: app.export.timestamps,
                            speakers: app.export.speakers,
                            metadata: app.export.metadata,
                        };
                        // Left open when the destination needs confirming, so
                        // the second click lands on the same dialog.
                        if app.export_conversation(
                            conversation,
                            app.export.format.to_export(),
                            options,
                            &destination,
                        ) {
                            app.export.open = false;
                        }
                    }
                    if ui.button(t("Cancel")).clicked() {
                        app.export.open = false;
                        app.export.confirming = None;
                    }
                });
            });
    }
}

/// Expands a leading `~` so a hand-typed path behaves the way a shell would.
/// Nothing else is expanded: this is a path field, not a shell.
fn shellexpand_home(path: &str) -> String {
    let trimmed = path.trim();
    match trimmed.strip_prefix("~/") {
        Some(rest) => dirs::home_dir()
            .map(|home| home.join(rest).display().to_string())
            .unwrap_or_else(|| trimmed.to_owned()),
        None => trimmed.to_owned(),
    }
}
