//! Conversation detail: rename, group assignment, tag editing, and export.

use egui::RichText;
use fc_core::{ConversationId, ConversationStatus};

use crate::app::App;
use crate::i18n::t;

pub fn show(app: &mut App, ui: &mut egui::Ui, id: ConversationId) {
    let Some(index) = app.conversations.iter().position(|c| c.id == id) else {
        ui.weak(t("This conversation is gone."));
        return;
    };

    // Edited on a clone so the borrow checker doesn't have to referee
    // reading `app.groups`/`app.tags` while writing `app.conversations`;
    // written back at the end if anything changed.
    let mut conversation = app.conversations[index].clone();
    let mut changed = false;

    ui.horizontal(|ui| {
        ui.heading(t("Conversation"));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            crate::app::export_ui::button(app, ui, &conversation);
        });
    });
    ui.separator();

    ui.horizontal(|ui| {
        ui.label(t("Title"));
        changed |= ui.text_edit_singleline(&mut conversation.title).changed();
    });

    ui.horizontal(|ui| {
        ui.label(t("Group"));
        let current_name = conversation
            .group
            .and_then(|id| app.groups.iter().find(|group| group.id == id))
            .map_or_else(|| t("Ungrouped").to_owned(), |group| group.name.clone());
        egui::ComboBox::from_id_salt("history-group")
            .selected_text(current_name)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(conversation.group.is_none(), t("Ungrouped"))
                    .clicked()
                {
                    conversation.group = None;
                    changed = true;
                }
                for group in app.groups.clone() {
                    if ui
                        .selectable_label(conversation.group == Some(group.id), &group.name)
                        .clicked()
                    {
                        conversation.group = Some(group.id);
                        changed = true;
                    }
                }
            });
    });

    ui.label(t("Tags"));
    ui.horizontal_wrapped(|ui| {
        let mut current = app.conversation_tags.entry(id).or_default().clone();
        for tag in app.tags.clone() {
            let selected = current.contains(&tag.id);
            if ui.selectable_label(selected, &tag.name).clicked() {
                if selected {
                    current.retain(|existing| *existing != tag.id);
                } else {
                    current.push(tag.id);
                }
                app.persist_tags(id, &current);
                app.conversation_tags.insert(id, current.clone());
            }
        }
    });

    ui.add_space(8.0);
    ui.label(format!(
        "{} · {} ({}) · {}",
        conversation.source.label(),
        conversation.engine.engine,
        conversation.engine.model,
        status_label(conversation.status),
    ));
    if conversation.mic_track {
        ui.label(RichText::new(t("Microphone track captured")).color(app.palette.dim));
    }

    ui.separator();
    egui::ScrollArea::vertical()
        .id_salt("history-segments")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for segment in app.segments_for(id) {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(segment.speaker_label()).strong());
                    ui.label(&segment.text);
                });
            }
        });

    if changed {
        app.persist_conversation(&conversation);
        app.conversations[index] = conversation;
    }
}

fn status_label(status: ConversationStatus) -> &'static str {
    match status {
        ConversationStatus::Active => "active",
        ConversationStatus::Completed => "completed",
        ConversationStatus::Interrupted => "interrupted",
    }
}
