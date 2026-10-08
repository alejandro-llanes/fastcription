//! The word registry: expressions the reader did not know, what they mean,
//! and where they were said.
//!
//! Three pieces. The **selection** is made in the transcript itself
//! (`select.rs`): drag across the words, or double-click one, then `Ctrl+D`
//! or the right-click menu — because during a meeting nobody has a hand free
//! to type, and a form was the wrong shape for the moment. A lookup asks the
//! vocabulary first and the meaning server only for something new, and shows
//! the answer on a **card** beside the transcript. The **pane** is the
//! registry itself, newest first, with a paste-a-list import for the words
//! that arrive some other way. And the **lookups**, which run on threads and
//! land in the store when they answer — like the transcription itself, the
//! window never waits on the network.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use egui::{RichText, Ui};
use fc_core::{ConversationId, Word, WordId};

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
    /// The entry the meaning card is showing, while it is open.
    pub card: Option<WordId>,
    /// The paste-a-list import panel's text, while it is open.
    pub import: Option<String>,
    /// True for the frame after the panel opens, so the box takes focus once
    /// and then leaves the reader alone.
    pub import_fresh: bool,
    /// The entry whose fields are being edited inline, with the draft.
    pub editing: Option<Word>,
    pub search: String,
    /// When the model was last asked to stay loaded.
    pub warmed: Option<Instant>,
}

impl App {
    /// Adds an expression to the registry, and looks it up if asked.
    pub(super) fn add_word(
        &mut self,
        expression: &str,
        context: &str,
        conversation: Option<ConversationId>,
        start_ms: Option<u64>,
        look_up: bool,
    ) -> Option<WordId> {
        let expression = expression.trim();
        if expression.is_empty() {
            return None;
        }
        let store = self.store.clone()?;
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
                Some(id)
            }
            Err(err) => {
                self.notify(
                    NoticeKind::Error,
                    tf("Could not add the word: {}", &[&err.to_string()]),
                );
                None
            }
        }
    }

    /// What the row menu and `Ctrl+D` do with a selection.
    pub(super) fn apply_line_action(&mut self, action: super::transcript::LineAction) {
        use super::transcript::LineAction;
        match action {
            LineAction::LookUp(selection) => self.look_up_selection(&selection),
            LineAction::Add(selection) => {
                if let Some(id) = self.add_word(
                    &selection.text,
                    &selection.context,
                    selection.conversation,
                    selection.start_ms,
                    false,
                ) {
                    self.words.card = Some(id);
                }
            }
        }
    }

    /// `Ctrl+D`: whatever is selected in the transcript.
    pub(super) fn look_up_current_selection(&mut self) {
        match self.selection.clone() {
            Some(selection) => self.look_up_selection(&selection),
            None => self.notify(
                NoticeKind::Info,
                t("Select a word or expression in the transcript first — drag across it, or double-click a word.").to_owned(),
            ),
        }
    }

    /// The vocabulary first; the server only for something new.
    ///
    /// A word the reader already looked up is a word they already paid for,
    /// and the answer they edited is better than a fresh one from the model.
    /// Either way the card opens on it.
    pub(super) fn look_up_selection(&mut self, selection: &super::select::Selection) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let found = lock(&store).find_word(&selection.text);
        match found {
            Ok(Some(word)) => {
                self.words.card = Some(word.id);
                if !word.looked_up() {
                    self.look_up_word(word.id);
                }
            }
            Ok(None) => {
                if let Some(id) = self.add_word(
                    &selection.text,
                    &selection.context,
                    selection.conversation,
                    selection.start_ms,
                    true,
                ) {
                    self.words.card = Some(id);
                }
            }
            Err(err) => self.notify(
                NoticeKind::Error,
                tf("Could not read the word registry: {}", &[&err.to_string()]),
            ),
        }
    }

    /// A pasted list, one word or expression per line.
    ///
    /// Returns how many were new. Lines that were already there land on their
    /// existing entries and are not counted, which is what the store's
    /// `add_word` does on its own; here it is only tallied for the notice.
    pub(super) fn import_words(&mut self, text: &str, look_up: bool) -> usize {
        let lines = parse_import(text);
        let before = self.words.list.len();
        for expression in &lines {
            self.add_word(expression, "", None, None, false);
        }
        let added = self.words.list.len().saturating_sub(before);
        if look_up {
            self.look_up_missing_words();
        }
        self.notify(
            NoticeKind::Info,
            tf(
                "Added {} new words; {} were already in your vocabulary.",
                &[&added.to_string(), &(lines.len() - added).to_string()],
            ),
        );
        added
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

/// The lines of a pasted list: trimmed, blanks dropped, repeats dropped
/// case-insensitively, order kept.
pub fn parse_import(text: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    text.lines()
        .map(|l| {
            l.trim()
                .trim_matches(|c: char| c == ',' || c == ';' || c == '\u{2022}' || c == '-')
                .trim()
        })
        .filter(|l| !l.is_empty())
        .filter(|l| seen.insert(l.to_lowercase()))
        .map(str::to_owned)
        .collect()
}

// ------------------------------------------------------------- the card

/// The meaning of what was just looked up, beside the transcript.
///
/// A floating card rather than a trip to the Words view: the reader is in a
/// meeting and the transcript is what they are following. It stays until
/// closed, Escape, or the next lookup replaces it. In compact mode it is the
/// same card, small enough for the strip.
pub fn meaning_card(app: &mut App, ctx: &egui::Context) {
    let Some(id) = app.words.card else {
        return;
    };
    let Some(word) = app.words.list.iter().find(|w| w.id == id).cloned() else {
        app.words.card = None;
        return;
    };
    let palette = app.palette.clone();
    let busy = app.words.in_flight.contains_key(&id);
    let compact = app.compact;
    let mut close = false;
    let mut look_up = false;

    egui::Window::new("meaning-card")
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .anchor(
            egui::Align2::RIGHT_TOP,
            egui::vec2(-16.0, if compact { 8.0 } else { 72.0 }),
        )
        .frame(
            egui::Frame::default()
                .fill(palette.surface)
                .stroke(egui::Stroke::new(1.0, palette.accent.gamma_multiply(0.6)))
                .corner_radius(ui::PANEL_RADIUS)
                .inner_margin(egui::Margin::same(if compact { 10 } else { 14 }))
                .shadow(egui::epaint::Shadow {
                    offset: [0, 4],
                    blur: 18,
                    spread: 0,
                    color: palette.accent.gamma_multiply(0.18),
                }),
        )
        .show(ctx, |ui| {
            ui.set_max_width(if compact { 400.0 } else { 440.0 });
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(&word.expression)
                        .size(if compact { 16.0 } else { 20.0 })
                        .strong()
                        .color(palette.accent),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .small_button("\u{2715}")
                        .on_hover_text(t("Close (Esc)"))
                        .clicked()
                    {
                        close = true;
                    }
                });
            });
            if busy {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(14.0));
                    ui.label(
                        RichText::new(t("asking the meaning server\u{2026}"))
                            .color(palette.secondary),
                    );
                });
                return;
            }
            match (&word.translation, &word.meaning) {
                (None, None) => {
                    ui.label(
                        RichText::new(t("In your words, not looked up yet."))
                            .color(palette.secondary),
                    );
                    if ui.small_button(t("Look up")).clicked() {
                        look_up = true;
                    }
                }
                (translation, meaning) => {
                    if let Some(translation) = translation {
                        ui.label(
                            RichText::new(translation)
                                .size(if compact { 15.0 } else { 18.0 })
                                .color(palette.text),
                        );
                    }
                    if let Some(meaning) = meaning {
                        ui.label(RichText::new(meaning).color(palette.secondary));
                    }
                    if !compact {
                        if let Some(example) = &word.example {
                            ui.add_space(4.0);
                            ui.label(RichText::new(example).italics().color(palette.secondary));
                        }
                    }
                }
            }
        });

    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        close = true;
    }
    if look_up {
        app.look_up_word(id);
    }
    if close {
        app.words.card = None;
    }
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
                let importing = app.words.import.is_some();
                if ui::pill(
                    ui,
                    &palette,
                    if importing {
                        Tone::Primary
                    } else {
                        Tone::Normal
                    },
                    None,
                    t("Import"),
                )
                .on_hover_text(t("Paste a list of words or expressions (Ctrl+I)"))
                .clicked()
                {
                    app.words.import = if importing { None } else { Some(String::new()) };
                    app.words.import_fresh = !importing;
                }
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

        import_panel(app, ui, &palette);

        if app.words.list.is_empty() {
            ui.label(
                RichText::new(t(
                    "Nothing yet. Select a word or expression in the transcript \u{2014} drag \
                     across it, or double-click a word \u{2014} then press Ctrl+D or \
                     right-click it. Or paste a list with Import.",
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

/// The paste-a-list panel, while it is open.
///
/// One word or expression per line. `Ctrl+Enter` adds and looks them all up,
/// so a list pasted from somewhere else is a keyboard away from meanings.
fn import_panel(app: &mut App, ui: &mut Ui, palette: &crate::theme::Palette) {
    let Some(mut draft) = app.words.import.take() else {
        return;
    };
    let mut outcome: Option<bool> = None;
    let mut cancel = false;
    egui::Frame::default()
        .fill(palette.window)
        .stroke(egui::Stroke::new(1.0, palette.accent.gamma_multiply(0.5)))
        .corner_radius(ui::PANEL_RADIUS)
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui::label(ui, palette, t("import a list"));
            ui.add_space(4.0);
            let field = ui.add(
                egui::TextEdit::multiline(&mut draft)
                    .desired_rows(5)
                    .desired_width(f32::INFINITY)
                    .hint_text(t("One word or expression per line\u{2026}")),
            );
            if app.words.import_fresh {
                field.request_focus();
                app.words.import_fresh = false;
            }
            let count = parse_import(&draft).len();
            if field.has_focus()
                && ui.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Enter))
                && count > 0
            {
                outcome = Some(true);
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let n = count.to_string();
                if ui
                    .add_enabled(
                        count > 0,
                        egui::Button::new(tf("Add and look up {}", &[&n])),
                    )
                    .on_hover_text(t("Ctrl+Enter"))
                    .clicked()
                {
                    outcome = Some(true);
                }
                if ui
                    .add_enabled(count > 0, egui::Button::new(tf("Add {}", &[&n])))
                    .clicked()
                {
                    outcome = Some(false);
                }
                if ui.button(t("Cancel")).clicked() {
                    cancel = true;
                }
            });
        });
    ui.add_space(10.0);
    match outcome {
        Some(look_up) => {
            app.import_words(&draft, look_up);
        }
        None if cancel => {}
        None => app.words.import = Some(draft),
    }
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
                if !word.context.is_empty() {
                    ui.label(
                        RichText::new(format!("\u{201c}{}\u{201d}", word.context))
                            .small()
                            .color(palette.dim),
                    );
                }
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

    /// A list pasted from anywhere: bullets, trailing commas, blank lines,
    /// the same thing twice with different capitals.
    #[test]
    fn a_pasted_list_is_cleaned_and_deduplicated_in_order() {
        let text = "boil the ocean,\n\n- table this\n\u{2022} Ballpark figure\nBOIL THE OCEAN\n   \nask not;";
        assert_eq!(
            parse_import(text),
            ["boil the ocean", "table this", "Ballpark figure", "ask not"]
        );
    }

    #[test]
    fn an_empty_paste_is_an_empty_list() {
        assert!(parse_import("").is_empty());
        assert!(parse_import("\n  \n,\n").is_empty());
    }
}
