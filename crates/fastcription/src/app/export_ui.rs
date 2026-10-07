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
}

impl Default for State {
    fn default() -> Self {
        Self {
            format: Format::Text,
            open: false,
            timestamps: true,
            speakers: true,
            metadata: true,
        }
    }
}

/// The export button for the conversation currently open in the history
/// view. Opens a format picker; choosing one logs what would be written.
pub fn button(app: &mut App, ui: &mut egui::Ui, conversation: &fc_core::Conversation) {
    let clicked = ui
        .horizontal(|ui| {
            ui.add(crate::icons::Icon::Export.image(app.palette.text, 14.0));
            ui.button(t("Export")).clicked()
        })
        .inner;
    if clicked {
        app.export.open = !app.export.open;
    }
    if app.export.open {
        egui::Window::new(t("Export conversation"))
            .collapsible(false)
            .resizable(false)
            .show(ui.ctx(), |ui| {
                for format in Format::ALL {
                    ui.radio_value(&mut app.export.format, format, format.label());
                }
                ui.separator();
                ui.checkbox(&mut app.export.timestamps, t("Timestamps"));
                ui.checkbox(&mut app.export.speakers, t("Speaker labels"));
                ui.checkbox(&mut app.export.metadata, t("Metadata header"));
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button(t("Export")).clicked() {
                        let options = fc_export::ExportOptions {
                            timestamps: app.export.timestamps,
                            speakers: app.export.speakers,
                            metadata: app.export.metadata,
                        };
                        app.export_conversation(
                            conversation,
                            app.export.format.to_export(),
                            options,
                        );
                        app.export.open = false;
                    }
                    if ui.button(t("Cancel")).clicked() {
                        app.export.open = false;
                    }
                });
            });
    }
}
