//! The word registry: expressions the reader did not know, what they mean,
//! and where they were said.
//!
//! Three pieces. The **editor** is the small window that opens from a
//! transcript line's right-click menu (or `Ctrl+D` for the newest line): the
//! line's words as chips to click, so the expression is picked rather than
//! typed, because during a meeting nobody has a hand free to type. The
//! **pane** is the registry itself, newest first, each entry with its English
//! meaning beside its translation and a way back to the line it came from.
//! And the **lookups**, which run on threads against the meaning server and
//! land in the store when they answer — like the transcription itself, the
//! window never waits on the network.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use egui::{RichText, Ui};
use fc_core::{ConversationId, Segment, Word, WordId};

use crate::app::{App, NoticeKind, PendingDelete};
use crate::i18n::{t, tf};
use crate::lookup::{self, Answer};
use crate::session::lock;
use crate::ui::{self, Tone};

/// How often the model is asked to stay loaded while the app runs. Well
/// inside the hour the request asks for.
const REWARM_EVERY: Duration = Duration::from_secs(40 * 60);

#[derive(Default)]
pub struct State {
    /// Every entry, newest first, as the store has them.
    pub list: Vec<Word>,
    /// `false` until the store has been read once.
    pub loaded: bool,
    /// Lookups in flight, by the entry they are for.
    pub in_flight: HashMap<WordId, Receiver<Result<Answer, String>>>,
    /// The add-a-word window, while it is open.
    pub editor: Option<Editor>,
    /// The entry whose fields are being edited inline, with the draft.
    pub editing: Option<Word>,
    pub search: String,
    /// When the model was last asked to stay loaded.
    pub warmed: Option<Instant>,
}

/// The add-a-word window: a transcript line broken into chips, and the
/// expression they add up to.
pub struct Editor {
    /// The line, as said.
    pub context: String,
    /// Its words, in order, for the chips.
    pub words: Vec<String>,
    /// Which chips are lit.
    pub chosen: Vec<bool>,
    /// The expression, which the chips write and the reader may still edit.
    pub expression: String,
    pub conversation: Option<ConversationId>,
    pub start_ms: Option<u64>,
    /// Whether the expression field has been given focus yet.
    pub focused: bool,
}

impl Editor {
    pub fn for_line(
        text: &str,
        conversation: Option<ConversationId>,
        start_ms: Option<u64>,
    ) -> Self {
        let words: Vec<String> = text.split_whitespace().map(str::to_owned).collect();
        let chosen = vec![false; words.len()];
        Self {
            context: fc_core::single_line(text),
            words,
            chosen,
            expression: String::new(),
            conversation,
            start_ms,
            focused: false,
        }
    }

    /// The lit chips, in line order, with the punctuation that clings to a
    /// word's ends dropped: "ocean," is the word "ocean".
    pub fn expression_from_chips(&self) -> String {
        self.words
            .iter()
            .zip(&self.chosen)
            .filter(|(_, &on)| on)
            .map(|(w, _)| trim_punctuation(w))
            .filter(|w| !w.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Strips the punctuation that clings to a spoken word in a transcript:
/// commas, full stops, quotes, brackets. Apostrophes inside a word stay —
/// "don't" is one word.
pub fn trim_punctuation(word: &str) -> &str {
    word.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'' && c != '-')
}

impl App {
    /// Opens the editor for a transcript line.
    pub(super) fn open_word_editor(
        &mut self,
        segment: &Segment,
        conversation: Option<ConversationId>,
    ) {
        self.words.editor = Some(Editor::for_line(
            &segment.text,
            conversation,
            Some(segment.start_ms),
        ));
    }

    /// `Ctrl+D`: the newest settled line, which is the one just heard.
    pub(super) fn open_word_editor_for_latest(&mut self) {
        let Some(segment) = self.segments.last().cloned() else {
            self.notify(
                NoticeKind::Info,
                t("Nothing has been transcribed yet to add a word from."),
            );
            return;
        };
        let conversation = self.live_conversation;
        self.open_word_editor(&segment, conversation);
    }

    /// Adds an expression to the registry, and looks it up if asked.
    pub(super) fn add_word(
        &mut self,
        expression: &str,
        context: &str,
        conversation: Option<ConversationId>,
        start_ms: Option<u64>,
        look_up: bool,
    ) {
        let expression = expression.trim();
        if expression.is_empty() {
            return;
        }
        let Some(store) = self.store.clone() else {
            return;
        };
        let new = fc_store::NewWord {
            expression: expression.to_owned(),
            context: context.to_owned(),
            conversation,
            start_ms,
            created_at: super::now_millis(),
        };
        let outcome = lock(&store).add_word(&new);
        match outcome {
            Ok(id) => {
                self.reload_words();
                if look_up {
                    self.look_up_word(id);
                }
            }
            Err(err) => self.notify(
                NoticeKind::Error,
                tf("Could not add the word: {}", &[&err.to_string()]),
            ),
        }
    }

    /// Asks the meaning server about one entry, on a thread.
    pub(super) fn look_up_word(&mut self, id: WordId) {
        if self.words.in_flight.contains_key(&id) {
            return;
        }
        let Some(word) = self.words.list.iter().find(|w| w.id == id).cloned() else {
            return;
        };
        let config = self.lookup_config();
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("fc-word-lookup".into())
            .spawn(move || {
                let _ = tx.send(lookup::look_up(&config, &word.expression, &word.context));
            });
        match spawned {
            Ok(_) => {
                self.words.in_flight.insert(id, rx);
            }
            Err(err) => self.notify(
                NoticeKind::Error,
                tf("Could not start the lookup: {}", &[&err.to_string()]),
            ),
        }
    }

    /// Every entry without an answer yet, for the pane's one-click catch-up
    /// after a meeting.
    pub(super) fn look_up_missing_words(&mut self) {
        let missing: Vec<WordId> = self
            .words
            .list
            .iter()
            .filter(|w| !w.looked_up())
            .map(|w| w.id)
            .collect();
        for id in missing {
            self.look_up_word(id);
        }
    }

    pub(super) fn lookup_config(&self) -> lookup::Config {
        lookup::Config {
            endpoint: self.settings.lookup_endpoint.clone(),
            model: self.settings.lookup_model.clone(),
            language: self.settings.words_language.clone(),
        }
    }

    /// Collects finished lookups, loads the registry on the first frame that
    /// has a store, and keeps the model warm. Called every frame.
    pub(super) fn poll_words(&mut self) {
        if !self.words.loaded && self.store.is_some() {
            self.reload_words();
        }

        // One `try_recv` per lookup: an answer or a dead sender is collected,
        // a lookup still running is left alone.
        let mut done: Vec<(WordId, Option<Result<Answer, String>>)> = Vec::new();
        for (id, rx) in &self.words.in_flight {
            match rx.try_recv() {
                Ok(result) => done.push((*id, Some(result))),
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => done.push((*id, None)),
            }
        }
        for (id, result) in done {
            self.words.in_flight.remove(&id);
            match result {
                Some(Ok(answer)) => self.record_lookup(id, answer),
                Some(Err(err)) => self.notify(NoticeKind::Error, tf("Lookup failed: {}", &[&err])),
                None => self.notify(NoticeKind::Error, t("The lookup did not finish").to_owned()),
            }
        }

        if self.settings.lookup_keep_warm
            && self
                .words
                .warmed
                .is_none_or(|at| at.elapsed() > REWARM_EVERY)
        {
            self.words.warmed = Some(Instant::now());
            let config = self.lookup_config();
            let _ = std::thread::Builder::new()
                .name("fc-word-warm".into())
                .spawn(move || {
                    if let Err(err) = lookup::warm(&config) {
                        tracing::debug!(%err, "could not keep the meaning server's model loaded");
                    }
                });
        }
    }

    fn record_lookup(&mut self, id: WordId, answer: Answer) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let some = |s: &str| (!s.is_empty()).then(|| s.to_owned());
        let outcome = lock(&store).set_word_lookup(
            id,
            some(&answer.meaning).as_deref(),
            some(&answer.translation).as_deref(),
            some(&answer.example).as_deref(),
        );
        match outcome {
            Ok(()) => self.reload_words(),
            Err(err) => self.notify(
                NoticeKind::Error,
                tf("Could not save the lookup: {}", &[&err.to_string()]),
            ),
        }
    }

    pub(super) fn reload_words(&mut self) {
        let Some(store) = self.store.clone() else {
            return;
        };
        // Bound first so the store's guard is released before `notify`
        // wants the app.
        let outcome = lock(&store).list_words();
        match outcome {
            Ok(list) => {
                self.words.list = list;
                self.words.loaded = true;
            }
            Err(err) => self.notify(
                NoticeKind::Error,
                tf("Could not read the word registry: {}", &[&err.to_string()]),
            ),
        }
    }

    pub(super) fn save_word_edit(&mut self, word: Word) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let outcome = lock(&store).update_word(&word);
        match outcome {
            Ok(()) => {
                self.words.editing = None;
                self.reload_words();
            }
            Err(err) => self.notify(
                NoticeKind::Error,
                tf("Could not save the word: {}", &[&err.to_string()]),
            ),
        }
    }

    pub(super) fn delete_word(&mut self, id: WordId) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let outcome = lock(&store).delete_word(id);
        match outcome {
            Ok(()) => self.reload_words(),
            Err(err) => self.notify(
                NoticeKind::Error,
                tf("Could not delete the word: {}", &[&err.to_string()]),
            ),
        }
    }

    /// The settings pane's test button, on a thread like the transcription
    /// server's.
    pub(super) fn probe_lookup_server(&mut self) {
        let config = self.lookup_config();
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("fc-lookup-probe".into())
            .spawn(move || {
                let _ = tx.send(lookup::check(&config));
            });
        match spawned {
            Ok(_) => {
                self.settings.lookup_probe = None;
                self.settings.lookup_probe_rx = Some(rx);
            }
            Err(err) => {
                self.settings.lookup_probe =
                    Some(Err(format!("Could not run the connection test: {err}")));
            }
        }
    }

    pub(super) fn poll_lookup_probe(&mut self) {
        let Some(rx) = &self.settings.lookup_probe_rx else {
            return;
        };
        match rx.try_recv() {
            Ok(result) => {
                self.settings.lookup_probe = Some(result);
                self.settings.lookup_probe_rx = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.settings.lookup_probe_rx = None;
                if self.settings.lookup_probe.is_none() {
                    self.settings.lookup_probe =
                        Some(Err("The connection test did not finish".to_owned()));
                }
            }
        }
    }
}

// ----------------------------------------------------------- the editor

/// The add-a-word window, if one is open. Called every frame.
pub fn editor_window(app: &mut App, ctx: &egui::Context) {
    let Some(mut editor) = app.words.editor.take() else {
        return;
    };
    let palette = app.palette.clone();
    let mut outcome: Option<Outcome> = None;

    egui::Window::new(t("Add to my words"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            ui.set_width(520.0);
            ui::label(ui, &palette, t("tap the words of the expression"));
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                let mut changed = false;
                for (word, on) in editor.words.iter().zip(editor.chosen.iter_mut()) {
                    let tone = if *on { Tone::Primary } else { Tone::Normal };
                    if ui::pill(ui, &palette, tone, None, word).clicked() {
                        *on = !*on;
                        changed = true;
                    }
                }
                if changed {
                    editor.expression = editor.expression_from_chips();
                }
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(t("Expression"));
                let field = ui.add(
                    egui::TextEdit::singleline(&mut editor.expression)
                        .desired_width(300.0)
                        .hint_text(t("or type it")),
                );
                // Focused the frame the window opens, so the flow during a
                // meeting is Ctrl+D, type, Enter — nothing to click.
                if !editor.focused {
                    field.request_focus();
                    editor.focused = true;
                }
                if field.lost_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    && !editor.expression.trim().is_empty()
                {
                    outcome = Some(Outcome::Add { look_up: true });
                }
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let ready = !editor.expression.trim().is_empty();
                if ui
                    .add_enabled(ready, egui::Button::new(t("Add and look up")))
                    .clicked()
                {
                    outcome = Some(Outcome::Add { look_up: true });
                }
                if ui.add_enabled(ready, egui::Button::new(t("Add"))).clicked() {
                    outcome = Some(Outcome::Add { look_up: false });
                }
                if ui.button(t("Cancel")).clicked() {
                    outcome = Some(Outcome::Cancel);
                }
            });
        });

    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        outcome = Some(Outcome::Cancel);
    }
    match outcome {
        Some(Outcome::Add { look_up }) => {
            let expression = editor.expression.clone();
            app.add_word(
                &expression,
                &editor.context,
                editor.conversation,
                editor.start_ms,
                look_up,
            );
        }
        Some(Outcome::Cancel) => {}
        None => app.words.editor = Some(editor),
    }
}

enum Outcome {
    Add { look_up: bool },
    Cancel,
}

// ------------------------------------------------------------- the pane

pub fn show(app: &mut App, ui: &mut Ui) {
    let palette = app.palette.clone();
    ui::pane(ui, &palette, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui::label(ui, &palette, t("my words"));
            ui.add_space(6.0);
            ui.label(
                RichText::new(app.words.list.len().to_string())
                    .size(ui::LABEL_PT + 1.0)
                    .color(palette.dim),
            );
            ui.add_space(10.0);
            ui.add(
                egui::TextEdit::singleline(&mut app.words.search)
                    .desired_width(220.0)
                    .hint_text(t("Search…")),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let missing = app.words.list.iter().filter(|w| !w.looked_up()).count();
                if missing > 0
                    && ui::pill(
                        ui,
                        &palette,
                        Tone::Normal,
                        None,
                        &tf("Look up {} missing", &[&missing.to_string()]),
                    )
                    .clicked()
                {
                    app.look_up_missing_words();
                }
            });
        });
        ui.add_space(10.0);

        if app.words.list.is_empty() {
            ui.label(
                RichText::new(t(
                    "Nothing yet. Right-click a line of the transcript and choose \
                     \u{201c}Add to my words\u{201d}, or press Ctrl+D for the newest line.",
                ))
                .color(palette.secondary),
            );
            return;
        }

        let needle = app.words.search.trim().to_lowercase();
        let shown: Vec<Word> = app
            .words
            .list
            .iter()
            .filter(|w| {
                needle.is_empty()
                    || w.expression.to_lowercase().contains(&needle)
                    || w.translation
                        .as_deref()
                        .is_some_and(|s| s.to_lowercase().contains(&needle))
                    || w.meaning
                        .as_deref()
                        .is_some_and(|s| s.to_lowercase().contains(&needle))
            })
            .cloned()
            .collect();

        let mut action: Option<Action> = None;
        egui::ScrollArea::vertical()
            .id_salt("words-list")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for word in &shown {
                    entry(app, ui, &palette, word, &mut action);
                    ui.add_space(8.0);
                }
            });
        if let Some(action) = action {
            match action {
                Action::LookUp(id) => app.look_up_word(id),
                Action::Edit(word) => app.words.editing = Some(word),
                Action::Save(word) => app.save_word_edit(word),
                Action::CancelEdit => app.words.editing = None,
                Action::Delete(id) => app.ask_to_delete(PendingDelete::Word(id)),
                Action::Jump(conversation, start_ms) => {
                    app.open_history_at(conversation, start_ms);
                }
            }
        }
    });
}

enum Action {
    LookUp(WordId),
    Edit(Word),
    Save(Word),
    CancelEdit,
    Delete(WordId),
    Jump(ConversationId, u64),
}

/// One entry: the expression and its translation on the first line because
/// those are the pair the reader came for, the meaning under them in English
/// because it is what catches a literal translation, the example after that,
/// and the way back to where it was said.
fn entry(
    app: &App,
    ui: &mut Ui,
    palette: &crate::theme::Palette,
    word: &Word,
    action: &mut Option<Action>,
) {
    let editing = app
        .words
        .editing
        .as_ref()
        .filter(|e| e.id == word.id)
        .cloned();
    egui::Frame::default()
        .fill(palette.window)
        .stroke(egui::Stroke::new(1.0, palette.outline))
        .corner_radius(ui::CONTROL_RADIUS + 2)
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if let Some(mut draft) = editing {
                for (name, field) in [
                    (t("Expression"), &mut draft.expression),
                    (
                        t("Translation"),
                        draft.translation.get_or_insert_with(String::new),
                    ),
                    (t("Meaning"), draft.meaning.get_or_insert_with(String::new)),
                    (t("Example"), draft.example.get_or_insert_with(String::new)),
                ] {
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [90.0, 18.0],
                            egui::Label::new(RichText::new(name).color(palette.secondary)),
                        );
                        ui.add(egui::TextEdit::singleline(field).desired_width(f32::INFINITY));
                    });
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button(t("Save")).clicked() {
                        // Blank fields go back to "not answered" rather than
                        // being saved as empty strings.
                        let tidy = |s: Option<String>| s.filter(|s| !s.trim().is_empty());
                        draft.translation = tidy(draft.translation.take());
                        draft.meaning = tidy(draft.meaning.take());
                        draft.example = tidy(draft.example.take());
                        *action = Some(Action::Save(draft.clone()));
                    }
                    if ui.button(t("Cancel")).clicked() {
                        *action = Some(Action::CancelEdit);
                    }
                });
                return;
            }

            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(&word.expression)
                        .size(18.0)
                        .strong()
                        .color(palette.accent),
                );
                if let Some(translation) = &word.translation {
                    ui.add_space(8.0);
                    ui.label(RichText::new(translation).size(17.0).color(palette.text));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if ui.small_button(t("Delete")).clicked() {
                        *action = Some(Action::Delete(word.id));
                    }
                    if ui.small_button(t("Edit")).clicked() {
                        *action = Some(Action::Edit(word.clone()));
                    }
                    if app.words.in_flight.contains_key(&word.id) {
                        ui.add(egui::Spinner::new().size(14.0));
                        ui.label(
                            RichText::new(t("looking up…"))
                                .small()
                                .color(palette.secondary),
                        );
                    } else {
                        let label = if word.looked_up() {
                            t("Look up again")
                        } else {
                            t("Look up")
                        };
                        if ui.small_button(label).clicked() {
                            *action = Some(Action::LookUp(word.id));
                        }
                    }
                });
            });
            match &word.meaning {
                Some(meaning) => {
                    ui.label(RichText::new(meaning).color(palette.secondary));
                }
                None if !app.words.in_flight.contains_key(&word.id) => {
                    ui.label(
                        RichText::new(t("Not looked up yet."))
                            .small()
                            .color(palette.dim),
                    );
                }
                None => {}
            }
            if let Some(example) = &word.example {
                ui.label(RichText::new(example).italics().color(palette.secondary));
            }
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!("\u{201c}{}\u{201d}", word.context))
                        .small()
                        .color(palette.dim),
                );
                if let Some(conversation) = word.conversation {
                    let title = app
                        .conversations
                        .iter()
                        .find(|c| c.id == conversation)
                        .map(|c| c.title.clone())
                        .unwrap_or_else(|| t("the conversation").to_owned());
                    if ui
                        .link(RichText::new(tf("in {}", &[&title])).small())
                        .clicked()
                    {
                        *action = Some(Action::Jump(conversation, word.start_ms.unwrap_or(0)));
                    }
                }
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chips_add_up_to_the_expression_in_line_order_without_clinging_punctuation() {
        let mut editor = Editor::for_line("I don't want to boil the ocean, honestly.", None, None);
        assert_eq!(editor.words.len(), 8);
        // Lit out of order on purpose: the expression follows the line.
        editor.chosen[6] = true; // ocean,
        editor.chosen[4] = true; // boil
        editor.chosen[5] = true; // the
        assert_eq!(editor.expression_from_chips(), "boil the ocean");
    }

    #[test]
    fn punctuation_is_trimmed_but_apostrophes_and_hyphens_inside_a_word_stay() {
        assert_eq!(trim_punctuation("ocean,"), "ocean");
        assert_eq!(trim_punctuation("\u{201c}table\u{201d}"), "table");
        assert_eq!(trim_punctuation("don't"), "don't");
        assert_eq!(trim_punctuation("ballpark-ish."), "ballpark-ish");
        assert_eq!(trim_punctuation("..."), "");
    }

    #[test]
    fn the_context_is_kept_on_one_line() {
        let editor = Editor::for_line("first\nsecond  third", None, Some(5));
        assert_eq!(editor.context, "first second third");
        assert_eq!(editor.start_ms, Some(5));
    }
}
