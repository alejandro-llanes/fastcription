//! Conversation detail: rename, group assignment, tag editing, and export.

use egui::RichText;
use fc_core::{ConversationId, ConversationStatus};

use crate::app::{transcript, App, PendingDelete};
use crate::i18n::t;

pub fn show(app: &mut App, ui: &mut egui::Ui, id: ConversationId) {
    let palette = app.palette.clone();
    crate::ui::pane(ui, &palette, |ui| inner(app, ui, id));
}

fn inner(app: &mut App, ui: &mut egui::Ui, id: ConversationId) {
    let Some(index) = app.conversations.iter().position(|c| c.id == id) else {
        ui.weak(t("This conversation is gone."));
        return;
    };

    // Edited on a clone so the borrow checker doesn't have to referee
    // reading `app.groups`/`app.tags` while writing `app.conversations`;
    // written back at the end if anything changed.
    let mut conversation = app.conversations[index].clone();
    let mut changed = false;

    let recording = app.is_recording(conversation.id);
    ui.horizontal(|ui| {
        crate::ui::label(ui, &app.palette, t("conversation"));
        if recording {
            crate::ui::badge(ui, t("REC"), app.palette.accent);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // No Delete while this is the row the session is appending to: the
            // store refuses it, and offering a button that cannot work is
            // worse than not offering one.
            if !recording
                && ui
                    .button(t("Delete"))
                    .on_hover_text(t("Deletes this conversation and its transcript."))
                    .clicked()
            {
                app.ask_to_delete(PendingDelete::Conversation(conversation.id));
            }
            crate::app::export_ui::button(app, ui, &conversation);
            transcript::copy_buttons(ui, Some(&conversation), app.segments_for(id));
        });
    });
    // When it happened and how long it ran, which is what an export header
    // carries and what the window showed nowhere: a conversation page used to
    // name its source and its engine and never its date.
    ui.label(
        RichText::new(header_line(&conversation))
            .small()
            .color(app.palette.secondary),
    );
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

    if conversation.mic_track {
        ui.label(
            RichText::new(t("Microphone track captured"))
                .small()
                .color(app.palette.secondary),
        );
    }

    ui.separator();
    let pt = app.settings.transcript_pt;
    let target = app
        .pending_scroll
        .and_then(|(pending, start_ms)| (pending == id).then_some(start_ms));
    {
        // Scoped so this borrow of the transcript cache ends before the scroll
        // request is cleared below. Not cloned: a three-hour meeting is
        // thousands of segments and this runs every frame.
        let segments = app.segments_for(id);
        // The segment the search excerpt came from: the first one that has not
        // finished by then, so an offset that lands in a gap between two
        // utterances scrolls to the one that follows it.
        let target_row = target.and_then(|start_ms| {
            segments
                .iter()
                .position(|segment| segment.end_ms >= start_ms)
        });
        egui::ScrollArea::vertical()
            .id_salt("history-segments")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (row, segment) in segments.iter().enumerate() {
                    let response = transcript::row(ui, &app.palette, segment, pt);
                    if Some(row) == target_row {
                        response.scroll_to_me(Some(egui::Align::Center));
                    }
                    transcript::line_menu(&response, segment);
                }
                if segments.is_empty() {
                    ui.weak(t("This conversation has no transcript."));
                }
            });
    }
    // Dropped whether or not a row matched, so an offset that names nothing in
    // this transcript does not leave the view scrolling for ever.
    if target.is_some() {
        app.pending_scroll = None;
    }

    if changed {
        app.persist_conversation(&conversation);
        app.conversations[index] = conversation;
    }
}

/// The one line that says what this conversation is: when, how long, from
/// where, with what.
fn header_line(conversation: &fc_core::Conversation) -> String {
    let mut parts = vec![fc_core::time::local_datetime(conversation.started_at)];
    if let Some(ms) = conversation.duration_ms() {
        parts.push(fc_core::time::duration_hms(ms));
    }
    parts.push(conversation.source.label());
    parts.push(format!(
        "{} ({})",
        conversation.engine.engine, conversation.engine.model
    ));
    parts.push(status_label(conversation.status).to_owned());
    parts.join(" \u{b7} ")
}

fn status_label(status: ConversationStatus) -> &'static str {
    match status {
        ConversationStatus::Active => "active",
        ConversationStatus::Completed => "completed",
        ConversationStatus::Interrupted => "interrupted",
    }
}
