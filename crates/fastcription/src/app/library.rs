//! Everything the app does to the conversation library: renames, groups,
//! tags, deletion, search, import and export.
//!
//! Split out of `app/mod.rs` because it is the half of `App` that touches
//! `fc-store`, and reading it next to the recording path only obscured both.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use fc_core::{Conversation, ConversationId, TagId};
use fc_store::StoreError;

use crate::app::{App, LabelEdit, MainView, NoticeKind, PendingDelete, SEARCH_LIMIT};
use crate::i18n::{t, tf};
use crate::session::lock;

/// How long the search box waits after the last keystroke.
///
/// The search runs an FTS5 query and, now, a `get_conversation` per hit outside
/// the loaded page. Running that on every character meant a query per keypress
/// while the user was still typing the first word.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(200);

/// An import running on its own thread.
pub struct Running {
    result: mpsc::Receiver<Result<crate::env::Imported, String>>,
    /// Meetings added so far, bumped by the worker. Cheap progress: an import
    /// of a long history otherwise looks like the button did nothing.
    added: Arc<AtomicUsize>,
}

impl Running {
    pub fn added(&self) -> usize {
        self.added.load(Ordering::Relaxed)
    }
}

impl App {
    /// Writes an edited conversation back to the library.
    ///
    /// The history pane edits its own copy and hands it back; this is where
    /// that copy becomes durable. A failure raises a notice rather than being
    /// swallowed, because silently losing a rename is the kind of bug users
    /// cannot diagnose.
    pub(super) fn persist_conversation(&mut self, conversation: &Conversation) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let guard = lock(&store);
        if let Err(err) = guard.rename_conversation(conversation.id, &conversation.title) {
            drop(guard);
            self.notify(
                NoticeKind::Error,
                tf("Could not rename the conversation: {}", &[&err.to_string()]),
            );
            return;
        }
        if let Err(err) = guard.set_group(conversation.id, conversation.group) {
            drop(guard);
            self.notify(
                NoticeKind::Error,
                tf("Could not change the group: {}", &[&err.to_string()]),
            );
        }
    }

    /// Applies a tag change for one conversation.
    pub(super) fn persist_tags(&mut self, id: ConversationId, tags: &[TagId]) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let existing: Vec<TagId> = self.conversation_tags.get(&id).cloned().unwrap_or_default();
        let guard = lock(&store);
        let mut failure = None;
        for added in tags.iter().filter(|t| !existing.contains(t)) {
            if let Err(err) = guard.add_tag(id, *added) {
                failure = Some(err.to_string());
            }
        }
        for removed in existing.iter().filter(|t| !tags.contains(t)) {
            if let Err(err) = guard.remove_tag(id, *removed) {
                failure = Some(err.to_string());
            }
        }
        drop(guard);
        if let Some(message) = failure {
            self.notify(
                NoticeKind::Error,
                tf("Could not update tags: {}", &[&message]),
            );
        }
    }

    /// Creates a group and reloads the library so it appears everywhere at
    /// once: the sidebar filter and the history pane's assignment list.
    pub(super) fn create_group(&mut self, name: &str) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let outcome = lock(&store).create_group(name, super::now_millis());
        match outcome {
            Ok(_) => self.reload_library(),
            Err(err) => self.notify(
                NoticeKind::Error,
                tf("Could not create the group: {}", &[&err.to_string()]),
            ),
        }
    }

    /// Tag names are unique without regard to case in the store, so creating
    /// one that already exists returns the existing tag rather than failing.
    pub(super) fn create_tag(&mut self, name: &str) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let outcome = lock(&store).create_tag(name, None);
        match outcome {
            Ok(_) => self.reload_library(),
            Err(err) => self.notify(
                NoticeKind::Error,
                tf("Could not create the tag: {}", &[&err.to_string()]),
            ),
        }
    }

    /// Renames a group or a tag, or deletes one.
    ///
    /// Grouped under one method because each is a single statement plus a
    /// library reload, and the reload is the part that must not be forgotten:
    /// a renamed group that still shows its old name in the filter looks like
    /// the rename failed.
    pub(super) fn edit_label(&mut self, edit: LabelEdit) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let outcome = {
            let guard = lock(&store);
            match &edit {
                LabelEdit::RenameGroup(id, name) => guard.rename_group(*id, name),
                LabelEdit::DeleteGroup(id) => guard.delete_group(*id),
                LabelEdit::RenameTag(id, name) => guard.rename_tag(*id, name),
                LabelEdit::DeleteTag(id) => guard.delete_tag(*id),
            }
        };
        match outcome {
            Ok(()) => {
                // A deleted group may be the one being filtered on, and a
                // deleted tag may be in the tag filter; leaving either behind
                // would filter the list down to nothing with no visible cause.
                match edit {
                    LabelEdit::DeleteGroup(id) => {
                        if self.sidebar.group_filter == Some(id) {
                            self.sidebar.group_filter = None;
                        }
                    }
                    LabelEdit::DeleteTag(id) => {
                        self.sidebar.tag_filter.remove(&id);
                        for tags in self.conversation_tags.values_mut() {
                            tags.retain(|tag| *tag != id);
                        }
                    }
                    _ => {}
                }
                self.reload_library();
            }
            Err(err) => self.notify(
                NoticeKind::Error,
                tf("Could not apply that change: {}", &[&err.to_string()]),
            ),
        }
    }

    /// Deletes a conversation and its transcript. Confirmed in the UI first:
    /// this is the one irreversible thing the app can do to a user's data.
    pub(super) fn delete_conversation(&mut self, id: ConversationId) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let outcome = lock(&store).delete_conversation(id);
        match outcome {
            Ok(()) => {
                self.history_segments.remove(&id);
                if matches!(self.main_view, MainView::History(open) if open == id) {
                    self.main_view = MainView::Live;
                }
                self.reload_library();
            }
            // The store refuses to delete the row a session is appending to.
            // Worth saying plainly: "FOREIGN KEY constraint failed" would be
            // the alternative, and it explains nothing.
            Err(StoreError::ConversationActive { .. }) => self.notify(
                NoticeKind::Warning,
                t("That conversation is being recorded right now. Stop the recording first."),
            ),
            Err(err) => self.notify(
                NoticeKind::Error,
                tf("Could not delete the conversation: {}", &[&err.to_string()]),
            ),
        }
    }

    /// Routes every destructive action through the same confirmation.
    ///
    /// Groups and tags used to be deleted the instant the context-menu entry
    /// was clicked, which meant the two actions sitting one pixel below
    /// "Rename" were the two that could not be taken back — a deleted tag is
    /// gone from every conversation it was on.
    pub(super) fn ask_to_delete(&mut self, what: PendingDelete) {
        self.pending_delete = Some(what);
    }

    /// True while this conversation is the one being recorded, which is the
    /// one row that may not be deleted and the only `Active` one listed.
    pub(super) fn is_recording(&self, id: ConversationId) -> bool {
        self.recording == Some(id)
    }

    /// The confirmation every irreversible action goes through. A modal window
    /// rather than an inline button, so a mis-click on a list that has just
    /// reordered cannot destroy a transcript.
    pub(super) fn delete_confirmation(&mut self, ctx: &egui::Context) {
        let Some(what) = self.pending_delete else {
            return;
        };
        let (window, question, consequence) = self.delete_wording(what);

        let mut decision = None;
        egui::Window::new(window)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(question);
                ui.label(
                    egui::RichText::new(consequence)
                        .small()
                        .color(self.palette.secondary),
                );
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button(t("Delete")).clicked() {
                        decision = Some(true);
                    }
                    if ui.button(t("Cancel")).clicked() {
                        decision = Some(false);
                    }
                });
            });

        match decision {
            Some(true) => {
                self.pending_delete = None;
                match what {
                    PendingDelete::Conversation(id) => self.delete_conversation(id),
                    PendingDelete::Group(id) => self.edit_label(LabelEdit::DeleteGroup(id)),
                    PendingDelete::Tag(id) => self.edit_label(LabelEdit::DeleteTag(id)),
                }
            }
            Some(false) => self.pending_delete = None,
            None => {}
        }
    }

    /// The window title, the question and what the answer costs.
    ///
    /// Each one names the thing by the name the user gave it, and says what
    /// survives: deleting a group keeps its conversations (the column is
    /// `ON DELETE SET NULL`), and deleting a conversation does not.
    fn delete_wording(&self, what: PendingDelete) -> (&'static str, String, &'static str) {
        let quoted = |name: String| format!("\u{201c}{name}\u{201d}");
        match what {
            PendingDelete::Conversation(id) => {
                let name = self
                    .conversations
                    .iter()
                    .find(|c| c.id == id)
                    .map(|c| c.title.clone())
                    .unwrap_or_else(|| id.to_string());
                (
                    t("Delete conversation"),
                    format!("{} {}?", t("Permanently delete"), quoted(name)),
                    t("The transcript cannot be recovered."),
                )
            }
            PendingDelete::Group(id) => {
                let name = self
                    .groups
                    .iter()
                    .find(|g| g.id == id)
                    .map(|g| g.name.clone())
                    .unwrap_or_else(|| id.to_string());
                (
                    t("Delete group"),
                    format!("{} {}?", t("Delete the group"), quoted(name)),
                    t("Its conversations are kept, and become ungrouped."),
                )
            }
            PendingDelete::Tag(id) => {
                let name = self
                    .tags
                    .iter()
                    .find(|tag| tag.id == id)
                    .map(|tag| tag.name.clone())
                    .unwrap_or_else(|| id.to_string());
                (
                    t("Delete tag"),
                    format!("{} {}?", t("Delete the tag"), quoted(name)),
                    t("Its conversations are kept, and lose the tag."),
                )
            }
        }
    }

    /// Imports past voxtype meetings into the library, on a thread.
    ///
    /// Each meeting costs two `voxtype` subprocesses, so a history of fifty is
    /// tens of seconds. Running that inline froze the window for all of it,
    /// which during a meeting also froze the live transcript.
    pub(super) fn import_voxtype_meetings(&mut self) {
        if self.import.is_some() {
            return;
        }
        let Some(store) = self.store.clone() else {
            self.notify(
                NoticeKind::Error,
                "Importing needs the conversation library, which could not be opened.",
            );
            return;
        };
        if self.library_read_only {
            self.notify(
                NoticeKind::Error,
                "The conversation library is read-only, so nothing can be imported into it.",
            );
            return;
        }
        let Some(binary) = self.voxtype.clone() else {
            self.notify(
                NoticeKind::Error,
                "voxtype was not found, so there is nothing to import from.",
            );
            return;
        };

        let added = Arc::new(AtomicUsize::new(0));
        let progress = Arc::clone(&added);
        let (tx, result) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("fc-import".into())
            .spawn(move || {
                let _ = tx.send(crate::env::import_meetings(&store, &binary, &progress));
            });
        match spawned {
            Ok(_) => self.import = Some(Running { result, added }),
            Err(err) => self.notify(
                NoticeKind::Error,
                tf("Could not start the import: {}", &[&err.to_string()]),
            ),
        }
    }

    /// True while an import is running, so the button can say so.
    pub(super) fn import_running(&self) -> Option<usize> {
        self.import.as_ref().map(Running::added)
    }

    pub(super) fn poll_import(&mut self) {
        let Some(running) = &self.import else {
            return;
        };
        match running.result.try_recv() {
            Ok(Ok(outcome)) => {
                self.import = None;
                self.reload_library();
                let mut message = tf(
                    "Imported {} meeting(s), skipped {}.",
                    &[&outcome.added.to_string(), &outcome.skipped.to_string()],
                );
                let kind = if outcome.failed.is_empty() {
                    NoticeKind::Info
                } else {
                    message.push_str(&tf(
                        " {} could not be imported: {}",
                        &[
                            &outcome.failed.len().to_string(),
                            &outcome.failed.join("; "),
                        ],
                    ));
                    NoticeKind::Warning
                };
                self.notify(kind, message);
            }
            Ok(Err(err)) => {
                self.import = None;
                self.notify(NoticeKind::Error, err);
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.import = None;
                self.notify(NoticeKind::Error, "The import did not finish.");
            }
        }
    }

    /// Re-reads the library after it changes on disk.
    pub(super) fn reload_library(&mut self) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let library = crate::env::load_library(&store, self.recording);
        if let Some(problem) = library.problem {
            self.notify(NoticeKind::Error, problem);
            return;
        }
        self.conversations = library.value.conversations;
        self.groups = library.value.groups;
        self.tags = library.value.tags;
        self.conversation_tags = library.value.conversation_tags;
        // The conversation being recorded grows while it is open, so a cache
        // of its transcript is stale the moment it is taken — and exporting
        // from the history pane then produced a truncated transcript.
        if let Some(id) = self.recording {
            self.history_segments.remove(&id);
        }
    }

    /// Opens a conversation and asks the history view to scroll to the segment
    /// a search excerpt came from.
    ///
    /// Scrolling happens in the view, which is the only place that knows where
    /// each row ended up; this leaves the request behind for it to pick up.
    pub(super) fn open_history_at(&mut self, id: ConversationId, start_ms: u64) {
        self.open_history(id);
        self.pending_scroll = Some((id, start_ms));
    }

    pub(super) fn open_history(&mut self, id: ConversationId) {
        self.main_view = MainView::History(id);
        // The destination belongs to the conversation it was suggested for;
        // leaving it set wrote the next transcript over the previous one's
        // file, with its name.
        self.export.open = false;
        self.export.confirming = None;
        if let (Some(store), false) = (self.store.as_ref(), self.history_segments.contains_key(&id))
        {
            let segments = crate::env::load_segments(store, id);
            self.history_segments.insert(id, segments);
        }
    }

    pub(super) fn segments_for(&self, id: ConversationId) -> &[fc_core::Segment] {
        self.history_segments
            .get(&id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Runs the transcript search when the query has settled.
    ///
    /// The sidebar's box searches two things: conversation titles, matched in
    /// memory, and the words that were actually said, matched by SQLite's
    /// full-text index. Finding a meeting by something said in it is the whole
    /// point of keeping the transcripts.
    pub(super) fn update_search(&mut self, ctx: &egui::Context) {
        if self.sidebar.search == self.sidebar.searched_for {
            return;
        }
        // Debounced: one query per settled query, not one per keystroke.
        match self.sidebar.typed_at {
            Some(at) if at.elapsed() >= SEARCH_DEBOUNCE => {}
            Some(at) => {
                ctx.request_repaint_after(SEARCH_DEBOUNCE.saturating_sub(at.elapsed()));
                return;
            }
            None => {
                self.sidebar.typed_at = Some(Instant::now());
                ctx.request_repaint_after(SEARCH_DEBOUNCE);
                return;
            }
        }
        self.sidebar.typed_at = None;
        self.sidebar.searched_for = self.sidebar.search.clone();

        let query = self.sidebar.search.trim().to_owned();
        if query.is_empty() {
            self.sidebar.matches = None;
            self.search_extra.clear();
            return;
        }
        let Some(store) = self.store.clone() else {
            self.sidebar.matches = None;
            return;
        };

        let hits = lock(&store).search(&query, SEARCH_LIMIT);
        match hits {
            Ok(hits) => {
                let mut by_conversation = HashMap::new();
                for hit in hits {
                    // The first hit in rank order is the best excerpt for that
                    // conversation, so later ones do not overwrite it. Its
                    // offset is kept with it: an excerpt the user can click is
                    // only useful if it can open the transcript where the words
                    // actually are.
                    by_conversation
                        .entry(hit.conversation_id)
                        .or_insert(super::sidebar::Hit {
                            snippet: hit.snippet,
                            start_ms: hit.start_ms,
                        });
                }
                // A hit can be in any conversation, but the sidebar only holds
                // the most recent page: without this, searching a long-lived
                // library found the words and had no row to show them on.
                self.search_extra = by_conversation
                    .keys()
                    .filter(|id| !self.conversations.iter().any(|c| c.id == **id))
                    .filter_map(|id| crate::env::load_conversation(&store, *id))
                    .collect();
                self.search_extra.sort_by_key(|c| -c.started_at);
                self.sidebar.matches = Some(by_conversation);
            }
            Err(err) => {
                self.sidebar.matches = Some(HashMap::new());
                self.search_extra.clear();
                self.notify(
                    NoticeKind::Error,
                    tf("Search failed: {}", &[&err.to_string()]),
                );
            }
        }
    }

    /// Where a transcript is suggested to go: the user's downloads directory,
    /// named after the conversation.
    ///
    /// There is no file dialog: pulling in a portal dependency for this is not
    /// worth it yet, so the path is editable text and the result is reported
    /// back so the user knows exactly where it went.
    pub(super) fn default_export_path(
        &self,
        conversation: &Conversation,
        format: fc_export::ExportFormat,
    ) -> PathBuf {
        let dir = dirs::download_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(std::env::temp_dir);
        dir.join(format!(
            "{}.{}",
            sanitise_filename(&conversation.title),
            format.extension()
        ))
    }

    /// Writes a transcript to a file.
    ///
    /// Returns false when the write did not happen because the destination
    /// exists: the caller then shows "Replace?" and the next click on the same
    /// path goes through. Overwriting on the first click was silent data loss
    /// from a text field, and `create_dir_all` ran before that check, so a
    /// typo in the path left empty directories behind whether or not anything
    /// was written.
    pub(super) fn export_conversation(
        &mut self,
        conversation: &Conversation,
        format: fc_export::ExportFormat,
        options: fc_export::ExportOptions,
        destination: &Path,
    ) -> bool {
        let segments = self.segments_for(conversation.id).to_vec();
        if segments.is_empty() {
            self.notify(
                NoticeKind::Warning,
                "That conversation has no transcript to export.",
            );
            return true;
        }
        if destination.as_os_str().is_empty() {
            // The dialog stays open: the user has to be able to type the path
            // the notice just asked them for.
            self.notify(NoticeKind::Warning, "Choose where to save the transcript.");
            return false;
        }

        if destination.exists() && self.export.confirming.as_deref() != Some(destination) {
            self.export.confirming = Some(destination.to_path_buf());
            return false;
        }

        // Only once the destination is settled: a directory created for a path
        // the user then corrected is litter.
        if let Some(parent) = destination.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                if let Err(err) = std::fs::create_dir_all(parent) {
                    self.notify(
                        NoticeKind::Error,
                        tf(
                            "Could not create {}: {}",
                            &[&parent.display().to_string(), &err.to_string()],
                        ),
                    );
                    return true;
                }
            }
        }

        // Streamed rather than rendered into a `String` first: a three-hour
        // meeting is megabytes of text, and it is about to be bytes on disk
        // either way.
        let written = std::fs::File::create(destination).and_then(|file| {
            let mut out = std::io::BufWriter::new(file);
            fc_export::write(conversation, &segments, format, &options, &mut out)?;
            std::io::Write::flush(&mut out)
        });
        match written {
            Ok(()) => self.notify(
                NoticeKind::Info,
                tf("Exported to {}", &[&destination.display().to_string()]),
            ),
            Err(err) => self.notify(
                NoticeKind::Error,
                tf(
                    "Could not write {}: {}",
                    &[&destination.display().to_string(), &err.to_string()],
                ),
            ),
        };
        self.export.confirming = None;
        true
    }
}

/// Keeps a user-chosen title usable as a filename without surprising them with
/// a mangled name: separators and control characters go, everything else stays.
fn sanitise_filename(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '\0') {
                '-'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').to_owned();
    if trimmed.is_empty() {
        "conversation".to_owned()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::sanitise_filename;

    #[test]
    fn filenames_survive_a_title_with_separators() {
        assert_eq!(sanitise_filename("1:1 with Alice"), "1-1 with Alice");
        assert_eq!(sanitise_filename("a/b\\c"), "a-b-c");
        assert_eq!(sanitise_filename("   "), "conversation");
        assert_eq!(sanitise_filename("..."), "conversation");
    }
}
