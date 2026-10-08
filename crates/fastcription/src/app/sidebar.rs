//! Groups, a tag filter, a search box, and the conversation list. Selecting a
//! row switches the main pane to the history view.

use std::collections::{BTreeSet, HashMap};

use egui::RichText;
use fc_core::{ConversationId, GroupId, TagId};

use crate::app::{App, LabelEdit, MainView};
use crate::i18n::t;

/// Which label an inline rename is editing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Renaming {
    Group(GroupId),
    Tag(TagId),
}

#[derive(Default)]
pub struct State {
    pub search: String,
    pub group_filter: Option<GroupId>,
    pub tag_filter: BTreeSet<TagId>,
    /// Draft names for the two creation fields. Without somewhere to create a
    /// group or a tag, assigning one in the history pane has nothing to offer
    /// and both features are dead on a fresh library.
    pub new_group: String,
    pub new_tag: String,
    /// Transcript matches for the current search, keyed by conversation, with
    /// the excerpt that matched. `None` means no search is active — distinct
    /// from an empty map, which means a search that found nothing.
    ///
    /// Cached rather than queried per frame: the search runs against SQLite and
    /// egui repaints many times a second.
    /// The group or tag currently being renamed, with its draft name. One at
    /// a time: an inline editor is clearer than a text field inside a context
    /// menu, which closes the moment it loses focus.
    pub renaming: Option<(Renaming, String)>,
    pub matches: Option<HashMap<ConversationId, String>>,
    /// The query `matches` was computed for, so a repaint does not re-run it.
    pub searched_for: String,
    /// When the query last changed, for the debounce: the search is an FTS
    /// query plus a fetch per hit outside the loaded page, and running it on
    /// every keystroke meant a query per character.
    pub typed_at: Option<std::time::Instant>,
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    ui.heading(t("Conversations"));
    if ui
        .add(egui::TextEdit::singleline(&mut app.sidebar.search).hint_text(t("Search…")))
        .changed()
    {
        app.sidebar.typed_at = Some(std::time::Instant::now());
    }
    ui.add_space(6.0);

    ui.label(
        RichText::new(t("Groups"))
            .small()
            .color(app.palette.secondary),
    );
    ui.horizontal_wrapped(|ui| {
        if ui
            .selectable_label(app.sidebar.group_filter.is_none(), t("All"))
            .clicked()
        {
            app.sidebar.group_filter = None;
        }
        for group in app.groups.clone() {
            let selected = app.sidebar.group_filter == Some(group.id);
            let chip = ui.selectable_label(selected, &group.name);
            if chip.clicked() {
                app.sidebar.group_filter = if selected { None } else { Some(group.id) };
            }
            chip.context_menu(|ui| {
                if ui.button(t("Rename")).clicked() {
                    app.sidebar.renaming = Some((Renaming::Group(group.id), group.name.clone()));
                    ui.close();
                }
                if ui.button(t("Delete group")).clicked() {
                    app.edit_label(LabelEdit::DeleteGroup(group.id));
                    ui.close();
                }
            });
        }
    });
    if let Some(name) = creator(ui, &mut app.sidebar.new_group, t("New group…"), "new-group") {
        app.create_group(&name);
    }

    ui.add_space(6.0);
    ui.label(
        RichText::new(t("Tags"))
            .small()
            .color(app.palette.secondary),
    );
    ui.horizontal_wrapped(|ui| {
        for tag in app.tags.clone() {
            let selected = app.sidebar.tag_filter.contains(&tag.id);
            let color = tag
                .color
                .as_deref()
                .and_then(fastframe_theme::parse_color)
                .unwrap_or(app.palette.accent);
            let chip = ui.selectable_label(selected, RichText::new(&tag.name).color(color));
            if chip.clicked() {
                if selected {
                    app.sidebar.tag_filter.remove(&tag.id);
                } else {
                    app.sidebar.tag_filter.insert(tag.id);
                }
            }
            chip.context_menu(|ui| {
                if ui.button(t("Rename")).clicked() {
                    app.sidebar.renaming = Some((Renaming::Tag(tag.id), tag.name.clone()));
                    ui.close();
                }
                if ui.button(t("Delete tag")).clicked() {
                    app.edit_label(LabelEdit::DeleteTag(tag.id));
                    ui.close();
                }
            });
        }
    });
    if let Some(name) = creator(ui, &mut app.sidebar.new_tag, t("New tag…"), "new-tag") {
        app.create_tag(&name);
    }

    if let Some((target, mut draft)) = app.sidebar.renaming.clone() {
        ui.add_space(4.0);
        let mut finish = None;
        ui.horizontal(|ui| {
            let field = ui.add(
                egui::TextEdit::singleline(&mut draft)
                    .id_salt("rename-label")
                    .desired_width(130.0),
            );
            field.request_focus();
            let committed = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.small_button(t("Save")).clicked() || committed {
                finish = Some(true);
            }
            if ui.small_button(t("Cancel")).clicked() {
                finish = Some(false);
            }
        });
        match finish {
            Some(true) => {
                let name = draft.trim().to_owned();
                app.sidebar.renaming = None;
                if !name.is_empty() {
                    app.edit_label(match target {
                        Renaming::Group(id) => LabelEdit::RenameGroup(id, name),
                        Renaming::Tag(id) => LabelEdit::RenameTag(id, name),
                    });
                }
            }
            Some(false) => app.sidebar.renaming = None,
            None => app.sidebar.renaming = Some((target, draft)),
        }
    }

    ui.add_space(8.0);
    ui.separator();

    egui::ScrollArea::vertical()
        .id_salt("conversation-list")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let search = app.sidebar.search.to_lowercase();
            let mut shown = 0usize;
            // The loaded page, plus whatever the search found beyond it.
            let listed: Vec<fc_core::Conversation> = app
                .conversations
                .iter()
                .chain(app.search_extra.iter())
                .cloned()
                .collect();
            for conversation in listed {
                if let Some(filter) = app.sidebar.group_filter {
                    if conversation.group != Some(filter) {
                        continue;
                    }
                }
                if !app.sidebar.tag_filter.is_empty() {
                    let tags = app
                        .conversation_tags
                        .get(&conversation.id)
                        .cloned()
                        .unwrap_or_default();
                    if !app.sidebar.tag_filter.iter().all(|tag| tags.contains(tag)) {
                        continue;
                    }
                }
                if !search.is_empty() {
                    let in_title = conversation.title.to_lowercase().contains(&search);
                    let in_transcript = app
                        .sidebar
                        .matches
                        .as_ref()
                        .is_some_and(|hits| hits.contains_key(&conversation.id));
                    if !in_title && !in_transcript {
                        continue;
                    }
                }
                row(app, ui, &conversation);
                shown += 1;
            }
            if shown == 0 {
                ui.weak(if search.is_empty() {
                    t("No conversations yet. Choose a source and press Start.")
                } else {
                    t("Nothing matches that search.")
                });
            }
        });
}

fn row(app: &mut App, ui: &mut egui::Ui, conversation: &fc_core::Conversation) {
    let selected = matches!(app.main_view, MainView::History(id) if id == conversation.id);
    let response = ui.selectable_label(
        selected,
        format!("{}\n{}", conversation.title, conversation.source.label()),
    );
    // The conversation being recorded is the one row that is still growing,
    // and the one the store will refuse to delete.
    if app.is_recording(conversation.id) {
        ui.label(
            RichText::new(t("recording"))
                .small()
                .strong()
                .color(app.palette.danger),
        );
    }
    // The excerpt is why this row matched; without it a transcript hit looks
    // like an unexplained result.
    if let Some(excerpt) = app
        .sidebar
        .matches
        .as_ref()
        .and_then(|hits| hits.get(&conversation.id))
    {
        ui.label(
            RichText::new(excerpt)
                .small()
                .italics()
                .color(app.palette.secondary),
        );
    }
    if response.clicked() {
        app.open_history(conversation.id);
    }
}

/// A one-line name field that commits on Enter or on the button, and returns
/// the trimmed name once. Returns `None` while there is nothing to create, so
/// a stray repaint cannot create the same group twice.
fn creator(ui: &mut egui::Ui, draft: &mut String, hint: &str, id_salt: &str) -> Option<String> {
    let mut submitted = false;
    ui.horizontal(|ui| {
        let field = ui.add(
            egui::TextEdit::singleline(draft)
                .id_salt(id_salt)
                .hint_text(hint)
                .desired_width(120.0),
        );
        submitted = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if ui.small_button(t("+")).clicked() {
            submitted = true;
        }
    });

    if !submitted {
        return None;
    }
    let name = draft.trim().to_owned();
    draft.clear();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}
