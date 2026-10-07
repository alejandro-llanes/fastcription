//! Groups, a tag filter, a search box, and the conversation list. Selecting a
//! row switches the main pane to the history view.

use std::collections::BTreeSet;

use egui::RichText;
use fc_core::{GroupId, TagId};

use crate::app::{App, MainView};
use crate::i18n::t;

#[derive(Default)]
pub struct State {
    pub search: String,
    pub group_filter: Option<GroupId>,
    pub tag_filter: BTreeSet<TagId>,
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    ui.heading(t("Conversations"));
    ui.add(egui::TextEdit::singleline(&mut app.sidebar.search).hint_text(t("Search…")));
    ui.add_space(6.0);

    ui.label(RichText::new(t("Groups")).small().color(app.palette.secondary));
    ui.horizontal_wrapped(|ui| {
        if ui
            .selectable_label(app.sidebar.group_filter.is_none(), t("All"))
            .clicked()
        {
            app.sidebar.group_filter = None;
        }
        for group in app.groups.clone() {
            let selected = app.sidebar.group_filter == Some(group.id);
            if ui.selectable_label(selected, &group.name).clicked() {
                app.sidebar.group_filter = if selected { None } else { Some(group.id) };
            }
        }
    });

    ui.add_space(6.0);
    ui.label(RichText::new(t("Tags")).small().color(app.palette.secondary));
    ui.horizontal_wrapped(|ui| {
        for tag in app.tags.clone() {
            let selected = app.sidebar.tag_filter.contains(&tag.id);
            let color = tag
                .color
                .as_deref()
                .and_then(fastframe_theme::parse_color)
                .unwrap_or(app.palette.accent);
            if ui
                .selectable_label(selected, RichText::new(&tag.name).color(color))
                .clicked()
            {
                if selected {
                    app.sidebar.tag_filter.remove(&tag.id);
                } else {
                    app.sidebar.tag_filter.insert(tag.id);
                }
            }
        }
    });

    ui.add_space(8.0);
    ui.separator();

    egui::ScrollArea::vertical()
        .id_salt("conversation-list")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let search = app.sidebar.search.to_lowercase();
            for conversation in app.conversations.clone() {
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
                    if !app
                        .sidebar
                        .tag_filter
                        .iter()
                        .all(|tag| tags.contains(tag))
                    {
                        continue;
                    }
                }
                if !search.is_empty() && !conversation.title.to_lowercase().contains(&search) {
                    continue;
                }
                row(app, ui, &conversation);
            }
        });
}

fn row(app: &mut App, ui: &mut egui::Ui, conversation: &fc_core::Conversation) {
    let selected = matches!(app.main_view, MainView::History(id) if id == conversation.id);
    let response = ui.selectable_label(
        selected,
        format!("{}\n{}", conversation.title, conversation.source.label()),
    );
    if response.clicked() {
        app.open_history(conversation.id);
    }
}
