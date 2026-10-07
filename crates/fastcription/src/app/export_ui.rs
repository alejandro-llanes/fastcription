//! The export format picker. ARCHITECTURE.md §6 ties fastcription's export
//! formats to voxtype's own (`txt`, `md`, `json`, `srt`, `vtt`) so a
//! transcript from either is interchangeable downstream; `fc-export` owns
//! writing them; this button only chooses one and logs the choice until that
//! crate exists.

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
    const ALL: [Self; 5] = [
        Self::Text,
        Self::Markdown,
        Self::Json,
        Self::Srt,
        Self::Vtt,
    ];

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
}

impl Default for State {
    fn default() -> Self {
        Self {
            format: Format::Text,
            open: false,
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
                ui.horizontal(|ui| {
                    if ui.button(t("Export")).clicked() {
                        tracing::info!(
                            conversation = conversation.id.get(),
                            format = ?app.export.format,
                            "export requested (placeholder: fc-export is not wired in yet)"
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
