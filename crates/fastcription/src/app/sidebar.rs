//! Groups, a tag filter, a search box, and the conversation list. Selecting a
//! row switches the main pane to the history view.

use std::collections::{BTreeSet, HashMap};

use egui::text::{LayoutJob, TextFormat};
use egui::{Label, RichText, Sense};
use fc_core::{Conversation, ConversationId, GroupId, TagId, UnixMillis};

use crate::app::{App, LabelEdit, MainView, PendingDelete};
use crate::i18n::t;

/// Which label an inline rename is editing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Renaming {
    Group(GroupId),
    Tag(TagId),
}

/// One transcript match: the excerpt that matched, and where in the
/// conversation it was said. The offset is what makes the excerpt a link rather
/// than a caption — clicking it opens the transcript at those words.
pub struct Hit {
    pub snippet: String,
    pub start_ms: u64,
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
    /// The group or tag currently being renamed, with its draft name. One at
    /// a time: an inline editor is clearer than a text field inside a context
    /// menu, which closes the moment it loses focus.
    pub renaming: Option<(Renaming, String)>,
    /// Transcript matches for the current search, keyed by conversation.
    /// `None` means no search is active — distinct from an empty map, which
    /// means a search that found nothing.
    ///
    /// Cached rather than queried per frame: the search runs against SQLite and
    /// egui repaints many times a second.
    pub matches: Option<HashMap<ConversationId, Hit>>,
    /// The query `matches` was computed for, so a repaint does not re-run it.
    pub searched_for: String,
    /// When the query last changed, for the debounce: the search is an FTS
    /// query plus a fetch per hit outside the loaded page, and running it on
    /// every keystroke meant a query per character.
    pub typed_at: Option<std::time::Instant>,
}

/// Which heading a conversation is listed under.
///
/// "Tuesday's call" is how people name a meeting, and a flat list of titles
/// sorted by a timestamp nobody can see is no help in finding it. The buckets
/// are coarse on purpose: they answer "roughly when" at a glance, and the row
/// itself carries the time of day.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bucket {
    Today,
    Yesterday,
    ThisWeek,
    ThisMonth,
    Earlier,
}

impl Bucket {
    pub fn heading(self) -> &'static str {
        match self {
            Self::Today => "Today",
            Self::Yesterday => "Yesterday",
            Self::ThisWeek => "This week",
            Self::ThisMonth => "This month",
            Self::Earlier => "Earlier",
        }
    }
}

/// Which bucket an instant falls in, against the local calendar.
pub fn bucket(started_at: UnixMillis, now: UnixMillis) -> Bucket {
    bucket_at(started_at, now, local_offset())
}

/// Read per call and falling back to UTC, for the reason `fc_core::time`
/// documents: `time` refuses to determine the offset in a process that has
/// already started threads, which this one always has.
fn local_offset() -> time::UtcOffset {
    time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC)
}

/// Compared by **calendar day**, not by elapsed milliseconds: a meeting at
/// 23:50 is "yesterday" at 00:10 the next morning, not "today, twenty minutes
/// ago". The day count is taken from Julian day numbers so a month or year
/// boundary needs no special case.
fn bucket_at(started_at: UnixMillis, now: UnixMillis, offset: time::UtcOffset) -> Bucket {
    let (Some(then), Some(now)) = (at(started_at, offset), at(now, offset)) else {
        // An instant `time` cannot represent means a corrupt row. It sorts to
        // the bottom of the list anyway, so that is where its heading goes.
        return Bucket::Earlier;
    };
    let days = now.date().to_julian_day() - then.date().to_julian_day();
    if days <= 0 {
        // Negative means the clock stepped backwards, or a row written by a
        // machine whose clock is ahead. Still the most recent thing there is.
        Bucket::Today
    } else if days == 1 {
        Bucket::Yesterday
    } else if days < 7 {
        Bucket::ThisWeek
    } else if (then.year(), then.month()) == (now.year(), now.month()) {
        Bucket::ThisMonth
    } else {
        Bucket::Earlier
    }
}

fn at(millis: UnixMillis, offset: time::UtcOffset) -> Option<time::OffsetDateTime> {
    time::OffsetDateTime::from_unix_timestamp(millis.div_euclid(1000))
        .ok()
        .map(|dt| dt.to_offset(offset))
}

/// The two characters SQLite's `snippet()` wraps a match in.
const MARK_OPEN: char = '\u{2039}';
const MARK_CLOSE: char = '\u{203a}';

/// Splits a `snippet()` excerpt into runs of text, each flagged as matched or
/// not.
///
/// The markers used to be rendered as themselves, which put stray guillemets
/// in the middle of a sentence and left the user to work out which words the
/// search had actually found. An unbalanced marker — a transcript that happens
/// to contain one — degrades to a run that is highlighted to the end rather
/// than to anything worse.
pub fn marked_spans(snippet: &str) -> Vec<(String, bool)> {
    let mut spans: Vec<(String, bool)> = Vec::new();
    let mut current = String::new();
    let mut matched = false;
    for c in snippet.chars() {
        match c {
            MARK_OPEN | MARK_CLOSE => {
                if !current.is_empty() {
                    spans.push((std::mem::take(&mut current), matched));
                }
                matched = c == MARK_OPEN;
            }
            other => current.push(other),
        }
    }
    if !current.is_empty() {
        spans.push((current, matched));
    }
    spans
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
                    app.ask_to_delete(PendingDelete::Group(group.id));
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
            let chip = ui.selectable_label(
                selected,
                RichText::new(&tag.name).color(tag_color(app, &tag)),
            );
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
                    app.ask_to_delete(PendingDelete::Tag(tag.id));
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
            let now = super::now_millis();
            let mut shown = 0usize;
            let mut heading: Option<Bucket> = None;
            // The loaded page, plus whatever the search found beyond it.
            // Re-sorted: the store returns each of the two newest-first, and
            // the date headings below are emitted on a change of bucket, so a
            // merge that was only locally ordered would print "Today" twice.
            let mut listed: Vec<Conversation> = app
                .conversations
                .iter()
                .chain(app.search_extra.iter())
                .cloned()
                .collect();
            listed.sort_by_key(|conversation| -conversation.started_at);

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
                let bucket = bucket(conversation.started_at, now);
                if heading != Some(bucket) {
                    heading = Some(bucket);
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(t(bucket.heading()))
                            .small()
                            .strong()
                            .color(app.palette.secondary),
                    );
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

fn row(app: &mut App, ui: &mut egui::Ui, conversation: &Conversation) {
    let selected = matches!(app.main_view, MainView::History(id) if id == conversation.id);
    let response = ui
        .selectable_label(selected, row_job(app, ui, conversation))
        // The source is no longer on the row — three lines of title, time and
        // tags are what make a conversation findable, and the source is in the
        // history pane's own header — but it is still one hover away.
        .on_hover_text(conversation.source.label());
    if response.clicked() {
        app.open_history(conversation.id);
    }

    // Read out before anything mutable is touched, so the click below can take
    // `&mut App` without the borrow of `matches` still being alive.
    let hit = app
        .sidebar
        .matches
        .as_ref()
        .and_then(|hits| hits.get(&conversation.id))
        .map(|hit| (hit.snippet.clone(), hit.start_ms));
    // The excerpt is why this row matched; without it a transcript hit looks
    // like an unexplained result.
    if let Some((snippet, start_ms)) = hit {
        // Not selectable: a selectable label treats the press as the start of a
        // drag-select, and this one has a click to register. The cursor and the
        // tooltip are what say so, since a `LayoutJob`'s explicit colours mean
        // egui's own hover tint never applies.
        let clicked = ui
            .add(
                Label::new(excerpt_job(app, ui, &snippet))
                    .sense(Sense::click())
                    .selectable(false),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(t("Open the conversation at these words"))
            .clicked();
        if clicked {
            app.open_history_at(conversation.id, start_ms);
        }
    }
}

/// Title, then when and how long, then the tags — one galley, so the whole row
/// is one clickable, one-widget thing however many lines it takes.
fn row_job(app: &App, ui: &egui::Ui, conversation: &Conversation) -> LayoutJob {
    let body = egui::TextStyle::Body.resolve(ui.style());
    let small = egui::TextStyle::Small.resolve(ui.style());
    let mut job = LayoutJob::default();

    job.append(
        &conversation.title,
        0.0,
        TextFormat {
            color: app.palette.text,
            font_id: body,
            ..Default::default()
        },
    );
    // The conversation being recorded is the one row that is still growing,
    // and the one the store will refuse to delete.
    if app.is_recording(conversation.id) {
        job.append(
            &format!("  \u{25cf} {}", t("REC")),
            0.0,
            TextFormat {
                color: app.palette.danger,
                font_id: small.clone(),
                ..Default::default()
            },
        );
    }

    // Looked up here rather than inside `meta_line`, which is then a pure
    // function of what it prints and can be tested as one.
    let group = app
        .sidebar
        .group_filter
        .is_none()
        .then(|| {
            conversation
                .group
                .and_then(|id| app.groups.iter().find(|group| group.id == id))
                .map(|group| group.name.as_str())
        })
        .flatten();
    job.append(
        &format!(
            "\n{}",
            meta_line(group, conversation, app.is_recording(conversation.id))
        ),
        0.0,
        TextFormat {
            color: app.palette.secondary,
            font_id: small.clone(),
            ..Default::default()
        },
    );

    let tags: Vec<fc_core::Tag> = app
        .conversation_tags
        .get(&conversation.id)
        .map(|ids| {
            app.tags
                .iter()
                .filter(|tag| ids.contains(&tag.id))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    for (index, tag) in tags.iter().enumerate() {
        job.append(
            &if index == 0 {
                format!("\n{}", tag.name)
            } else {
                format!("  {}", tag.name)
            },
            0.0,
            TextFormat {
                color: tag_color(app, tag),
                font_id: small.clone(),
                ..Default::default()
            },
        );
    }
    job
}

/// `[group · ]time[ · duration]`, which is what tells two meetings with the
/// same default title apart.
///
/// `group` is `None` when there is none to name, and also when a group filter
/// is already on — every row would then repeat the name of the group the user
/// is looking at.
fn meta_line(group: Option<&str>, conversation: &Conversation, recording: bool) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(group) = group {
        parts.push(group.to_owned());
    }
    parts.push(fc_core::time::local_time(conversation.started_at));
    // No segment count: the store has no way to give one for a listed page
    // without a correlated `COUNT(*)` per row, which it dropped deliberately
    // for that reason, and its API is not ours to extend.
    match conversation.duration_ms() {
        Some(ms) => parts.push(fc_core::time::duration_hms(ms)),
        // No end time means this one is recording — the badge above already
        // says so — or that a process died mid-recording. Either way, counting
        // to now would claim a transcript that stopped yesterday is still
        // growing.
        None if !recording => parts.push(t("unfinished").to_owned()),
        None => {}
    }
    parts.join(" \u{b7} ")
}

/// The excerpt, with the words the search matched picked out.
fn excerpt_job(app: &App, ui: &egui::Ui, snippet: &str) -> LayoutJob {
    let small = egui::TextStyle::Small.resolve(ui.style());
    let mut job = LayoutJob::default();
    for (text, matched) in marked_spans(snippet) {
        job.append(
            &text,
            0.0,
            TextFormat {
                color: if matched {
                    app.palette.accent
                } else {
                    app.palette.secondary
                },
                italics: !matched,
                font_id: small.clone(),
                ..Default::default()
            },
        );
    }
    job
}

/// A tag's own colour, or the accent when it has none.
fn tag_color(app: &App, tag: &fc_core::Tag) -> egui::Color32 {
    tag.color
        .as_deref()
        .and_then(fastframe_theme::parse_color)
        .unwrap_or(app.palette.accent)
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

#[cfg(test)]
mod tests {
    use super::{bucket_at, marked_spans, meta_line, Bucket};
    use fc_core::{
        AudioSource, Conversation, ConversationId, ConversationStatus, EngineInfo, SourceKind,
    };
    use time::macros::datetime;
    use time::UtcOffset;

    fn conversation(started_at: i64, ended_at: Option<i64>) -> Conversation {
        Conversation {
            id: ConversationId(1),
            title: "Standup".into(),
            group: None,
            started_at,
            ended_at,
            status: ConversationStatus::Completed,
            source: AudioSource::named(SourceKind::SinkMonitor, "a.monitor", "A monitor"),
            mic_track: false,
            engine: EngineInfo {
                engine: "whisper".into(),
                model: "base.en".into(),
                language: "en".into(),
                backend: None,
            },
            voxtype_meeting_id: None,
        }
    }

    /// Local midnight on 2026-10-07, as the machine this is read on sees it.
    fn millis(dt: time::OffsetDateTime, offset: UtcOffset) -> i64 {
        dt.replace_offset(offset).unix_timestamp() * 1_000
    }

    /// Asserted for UTC and for two offsets either side of it, because the
    /// whole point of bucketing by calendar day is that it follows the user's
    /// day and not the epoch's.
    fn offsets() -> [UtcOffset; 3] {
        [
            UtcOffset::UTC,
            UtcOffset::from_hms(2, 0, 0).unwrap(),
            UtcOffset::from_hms(-7, 0, 0).unwrap(),
        ]
    }

    #[test]
    fn the_same_calendar_day_is_today() {
        for offset in offsets() {
            let now = millis(datetime!(2026-10-07 09:15:00 UTC), offset);
            for same_day in [
                datetime!(2026-10-07 00:00:00 UTC),
                datetime!(2026-10-07 09:14:59 UTC),
            ] {
                assert_eq!(
                    bucket_at(millis(same_day, offset), now, offset),
                    Bucket::Today,
                    "{same_day} at {offset}"
                );
            }
        }
    }

    /// Twenty minutes ago can be yesterday, and three hours ago can be today:
    /// elapsed time is the wrong question, which is why this is not a
    /// subtraction.
    #[test]
    fn just_before_midnight_is_yesterday_just_after_it() {
        for offset in offsets() {
            let now = millis(datetime!(2026-10-07 00:10:00 UTC), offset);
            assert_eq!(
                bucket_at(
                    millis(datetime!(2026-10-06 23:50:00 UTC), offset),
                    now,
                    offset
                ),
                Bucket::Yesterday
            );
            assert_eq!(
                bucket_at(
                    millis(datetime!(2026-10-07 00:01:00 UTC), offset),
                    now,
                    offset
                ),
                Bucket::Today
            );
        }
    }

    #[test]
    fn the_week_runs_out_after_six_days() {
        let offset = UtcOffset::UTC;
        let now = millis(datetime!(2026-10-20 12:00:00 UTC), offset);
        let bucket_of = |day| bucket_at(millis(day, offset), now, offset);
        assert_eq!(
            bucket_of(datetime!(2026-10-18 12:00:00 UTC)),
            Bucket::ThisWeek
        );
        // Six days back is the last day of "this week"; the seventh is not.
        assert_eq!(
            bucket_of(datetime!(2026-10-14 12:00:00 UTC)),
            Bucket::ThisWeek
        );
        assert_eq!(
            bucket_of(datetime!(2026-10-13 12:00:00 UTC)),
            Bucket::ThisMonth
        );
        assert_eq!(
            bucket_of(datetime!(2026-10-01 12:00:00 UTC)),
            Bucket::ThisMonth
        );
        // Same number of days back again, but the month has turned over.
        assert_eq!(
            bucket_of(datetime!(2026-09-30 12:00:00 UTC)),
            Bucket::Earlier
        );
    }

    /// A conversation four days back that happens to fall in the previous
    /// calendar month belongs under "This week", not under "Earlier": the
    /// week test runs first for exactly this case.
    #[test]
    fn a_month_boundary_does_not_hide_something_from_this_week() {
        let offset = UtcOffset::UTC;
        let now = millis(datetime!(2026-11-02 12:00:00 UTC), offset);
        assert_eq!(
            bucket_at(
                millis(datetime!(2026-10-30 12:00:00 UTC), offset),
                now,
                offset
            ),
            Bucket::ThisWeek
        );
        // Same day of the same month a year earlier is not "this month".
        assert_eq!(
            bucket_at(
                millis(datetime!(2025-11-01 12:00:00 UTC), offset),
                now,
                offset
            ),
            Bucket::Earlier
        );
    }

    /// A clock that stepped backwards, or a row written on a machine running
    /// ahead. It is still the newest thing in the list, so it is listed first,
    /// and a heading of "Earlier" above the top row would be absurd.
    #[test]
    fn a_future_timestamp_lands_under_today_rather_than_earlier() {
        let offset = UtcOffset::UTC;
        let now = millis(datetime!(2026-10-07 12:00:00 UTC), offset);
        assert_eq!(
            bucket_at(
                millis(datetime!(2026-10-09 12:00:00 UTC), offset),
                now,
                offset
            ),
            Bucket::Today
        );
    }

    #[test]
    fn an_unrepresentable_instant_gets_a_heading_instead_of_a_panic() {
        assert_eq!(bucket_at(i64::MIN, 0, UtcOffset::UTC), Bucket::Earlier);
        assert_eq!(bucket_at(0, i64::MAX, UtcOffset::UTC), Bucket::Earlier);
    }

    #[test]
    fn an_excerpt_separates_what_matched_from_what_did_not() {
        assert_eq!(
            marked_spans("we agreed \u{2039}to ship\u{203a} on Friday"),
            vec![
                ("we agreed ".to_owned(), false),
                ("to ship".to_owned(), true),
                (" on Friday".to_owned(), false),
            ]
        );
        // A match at the very start, and two of them.
        assert_eq!(
            marked_spans("\u{2039}ship\u{203a} it and \u{2039}ship\u{203a} it"),
            vec![
                ("ship".to_owned(), true),
                (" it and ".to_owned(), false),
                ("ship".to_owned(), true),
                (" it".to_owned(), false),
            ]
        );
    }

    /// The markers are ordinary characters that a transcript may contain. The
    /// worst that may happen is a run highlighted to the end of the excerpt —
    /// never a panic, and never a marker rendered as itself.
    #[test]
    fn an_unbalanced_marker_degrades_quietly() {
        assert_eq!(
            marked_spans("half \u{2039}open"),
            vec![("half ".to_owned(), false), ("open".to_owned(), true)]
        );
        assert_eq!(marked_spans(""), Vec::new());
        assert_eq!(marked_spans("\u{2039}\u{203a}"), Vec::new());
    }

    /// The line that answers "which of these is Tuesday's call": a time, a
    /// length, and the group when one is worth naming.
    #[test]
    fn the_meta_line_names_the_time_and_the_length() {
        let finished = conversation(1_791_383_422_000, Some(1_791_383_422_000 + 754_000));
        let time = fc_core::time::local_time(finished.started_at);
        assert_eq!(
            meta_line(None, &finished, false),
            format!("{time} \u{b7} 12:34")
        );
        assert_eq!(
            meta_line(Some("Work"), &finished, false),
            format!("Work \u{b7} {time} \u{b7} 12:34")
        );
    }

    /// Two reasons a row has no end time, and they must not read the same. The
    /// one being recorded carries its own badge, so a length there would be a
    /// second claim about the same thing; a row left behind by a crash has to
    /// say that it was never closed.
    #[test]
    fn a_row_with_no_end_says_which_kind_of_no_end_it_is() {
        let open = conversation(1_791_383_422_000, None);
        let time = fc_core::time::local_time(open.started_at);
        assert_eq!(meta_line(None, &open, true), time);
        assert_eq!(
            meta_line(None, &open, false),
            format!("{time} \u{b7} unfinished")
        );
    }
}
