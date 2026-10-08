//! App state and the types the views share.
//!
//! `App` owns everything: the running session, the library, and the per-pane
//! transient state. The behaviour is split by concern into sibling modules —
//! `chrome` draws the window, `session_control` runs a recording, `library`
//! talks to the store — so this file stays a description of the state rather
//! than of all of it at once.

mod chrome;
mod export_ui;
mod history;
mod library;
mod live;
mod readiness;
mod session_control;
mod settings;
mod sidebar;
mod transcript;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::Receiver;
use fastframe_theme::Palette as ThemePalette;

use fc_core::{
    AudioSource, Conversation, ConversationId, EngineInfo, Group, GroupId, Pressure, Segment,
    SessionEvent, SessionState, Tag, TagId, Track,
};

use crate::session::{self, Session, SharedStore};

/// How many transcript matches one search returns. Enough to cover any
/// realistic library without rendering a list nobody scrolls.
const SEARCH_LIMIT: u32 = 500;

/// How many notices are kept. Older ones are dropped: the list is for reading
/// what just went wrong, not for auditing a long session.
const MAX_NOTICES: usize = 50;

/// How long an `Info` notice stays before it dismisses itself. Long enough to
/// read "Exported to …", short enough not to sit over the transcript.
const INFO_LIFETIME: Duration = Duration::from_secs(6);

use crate::i18n::t;

/// Which main-pane view is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MainView {
    Live,
    History(ConversationId),
    Settings,
}

/// Stands in for the voxtype systemd user service's state (ARCHITECTURE.md
/// §7) until `fc-voxtype::service` is wired into the settings pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceStatus {
    Unknown,
    Running,
    Stopped,
}

/// What a confirmed deletion will delete.
///
/// One enum rather than three flags because all three go through the same
/// modal: a group and a tag used to be deleted straight from a context menu
/// while a conversation asked first, so the two destructive actions that are
/// easiest to hit by accident were the two that did not confirm.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PendingDelete {
    Conversation(ConversationId),
    Group(GroupId),
    Tag(TagId),
}

/// A rename or deletion of a group or a tag, applied by `App::edit_label`.
pub enum LabelEdit {
    RenameGroup(GroupId, String),
    DeleteGroup(GroupId),
    RenameTag(TagId, String),
    DeleteTag(TagId),
}

/// How loudly a notice asks to be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeKind {
    /// Something worked. Dismisses itself.
    Info,
    /// Something is degraded but the app carries on.
    Warning,
    /// Something the user has to act on. Stays until dismissed.
    Error,
}

/// Identifies one notice for as long as it is in the list, so an event that
/// undoes another's cause can take exactly that one away.
pub type NoticeId = u64;

pub struct Notice {
    pub id: NoticeId,
    pub at: Instant,
    pub kind: NoticeKind,
    pub text: String,
    /// How many times in a row this same text arrived. A transcriber failing
    /// every pass used to replace the banner once a second, which read as one
    /// problem flickering rather than as the same problem fifty times.
    pub count: u32,
}

/// The app's notice list: newest last, capped, with consecutive repeats
/// collapsed.
///
/// This replaced a single `Option<Banner>`, which could only ever show the
/// most recent problem — so a startup with no sound server *and* no voxtype
/// reported one of the two, and the next event erased whichever had been
/// shown. Successes went through the same slot and were painted in the danger
/// colour, which made "Exported to ~/Downloads/…" look like a failure.
#[derive(Default)]
pub struct Notices {
    items: VecDeque<Notice>,
    next_id: NoticeId,
}

impl Notices {
    /// Adds a notice, or counts a repeat of the newest one.
    pub fn push(&mut self, kind: NoticeKind, text: String) -> NoticeId {
        if let Some(last) = self.items.back_mut() {
            if last.text == text && last.kind == kind {
                last.count += 1;
                last.at = Instant::now();
                return last.id;
            }
        }
        let id = self.next_id;
        self.next_id += 1;
        self.items.push_back(Notice {
            id,
            at: Instant::now(),
            kind,
            text,
            count: 1,
        });
        while self.items.len() > MAX_NOTICES {
            self.items.pop_front();
        }
        id
    }

    pub fn newest(&self) -> Option<&Notice> {
        self.items.back()
    }

    /// Newest first, which is the order the expanded list reads in.
    pub fn iter_newest_first(&self) -> impl Iterator<Item = &Notice> {
        self.items.iter().rev()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn remove(&mut self, id: NoticeId) {
        self.items.retain(|notice| notice.id != id);
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Drops `Info` notices older than [`INFO_LIFETIME`] and returns when the
    /// next one expires, so the frame can ask for a repaint then instead of
    /// leaving a stale notice up until something else happens to repaint.
    pub fn expire(&mut self, now: Instant) -> Option<Duration> {
        // `saturating_duration_since`, not `-`: subtracting `Instant`s panics
        // on underflow, and a panic in a repaint is a lost meeting.
        self.items.retain(|notice| {
            notice.kind != NoticeKind::Info
                || now.saturating_duration_since(notice.at) < INFO_LIFETIME
        });
        self.items
            .iter()
            .filter(|notice| notice.kind == NoticeKind::Info)
            .map(|notice| INFO_LIFETIME.saturating_sub(now.saturating_duration_since(notice.at)))
            .min()
    }
}

/// Everything that should still be true the next time fastcription opens.
///
/// Deliberately small, and deliberately not the whole of `App`: a stale
/// conversation list or a half-finished session restored from disk would be
/// worse than nothing. The chosen source is stored as the descriptor it
/// already is (`fc_core::AudioSource` is serialisable precisely so this can be
/// resolved again) and re-resolved against the live list on the next launch.
///
/// `#[serde(default)]` so a blob written by an older build, missing a field
/// this one has, still restores the fields it does carry.
#[derive(Default, serde::Deserialize)]
#[serde(default)]
pub struct Persisted {
    pub settings: settings::State,
    /// The source being transcribed. Re-resolved by identity at startup; left
    /// unselected, with a notice, when it is gone.
    pub source: Option<AudioSource>,
    pub mic_source: Option<AudioSource>,
    pub mic_track: bool,
}

/// The write side of [`Persisted`], borrowing rather than cloning.
///
/// Two types rather than one because `settings::State` owns the channel of a
/// connection test in flight, which cannot be cloned — and giving it a `Clone`
/// that silently dropped a running test would be a trap. The field names and
/// order match `Persisted`, which is what RON needs to read it back.
#[derive(serde::Serialize)]
struct Saving<'a> {
    settings: &'a settings::State,
    source: Option<&'a AudioSource>,
    mic_source: Option<&'a AudioSource>,
    mic_track: bool,
}

/// The egui storage key. Named rather than defaulted so a future second blob
/// cannot silently collide with this one.
const PERSISTED_KEY: &str = "fastcription";

pub struct App {
    // Chrome: fastframe wiring that needs to run again on every reopened
    // window, since each one gets its own fresh `egui::Context`.
    theme_catalog: fastframe_theme::Catalog<crate::theme::Palette>,
    theme_waker: fastframe_theme::Waker,
    themes_dir: PathBuf,
    palette: crate::theme::Palette,
    text_rendering: fastframe_text::TextRendering,
    scrolling: fastframe_scroll::Scrolling,

    // Tray + shell: `closed()` hides to the tray instead of quitting when one
    // exists; `wants_show`/`quit_requested` cross from tray events (which can
    // arrive with no window open) into `Resident::headless_frame`.
    tray: Option<fastframe_tray::Tray>,
    wants_show: bool,
    quit_requested: bool,
    /// Set by the single-instance handler when a second launch asks for the
    /// window. Shared with that handler's thread, which has no way to reach
    /// `App` directly.
    show_requested: Arc<AtomicBool>,

    // The pipeline. `App` only ever drains `SessionEvent`s off `events` and
    // calls pause/resume/stop on `session`; it knows nothing about capture or
    // transcription. `events` is a handle to the running session's channel,
    // held separately so the tail of a finishing session still arrives after
    // `Session` itself has been consumed by `stop_async`.
    store: Option<SharedStore>,
    /// Where the library is and whether it will take writes, for the messages
    /// that have to name it.
    library_path: PathBuf,
    library_read_only: bool,
    captures: session::CaptureFactory,
    voxtype: Option<PathBuf>,
    engine: EngineInfo,
    session: Option<Session>,
    events: Option<Receiver<SessionEvent>>,
    /// The conversation being recorded, kept after `Session` is consumed by
    /// `stop_async` so the sidebar can mark it and the cached transcript can
    /// be invalidated when it closes.
    recording: Option<ConversationId>,
    /// Which conversation the live view's segments belong to. Unlike
    /// `recording` it survives the stop, because the transcript stays on screen
    /// after a session ends and exporting or copying it needs the record it was
    /// written to.
    live_conversation: Option<ConversationId>,
    /// This session's voxtype config, removed when the session ends.
    session_config: Option<PathBuf>,
    /// The startup probes, still running on their own thread.
    startup: Option<Receiver<crate::env::Startup>>,

    // Live session state
    state: SessionState,
    session_start: Option<Instant>,
    accumulated: Duration,
    pressure: Pressure,
    level_peak: f32,
    level_rms: f32,
    mic_track: bool,
    /// The microphone used for the optional second track. Separate from
    /// `sources`/`selected_source`, which is the source being transcribed.
    mic_source: AudioSource,
    sources: Vec<AudioSource>,
    /// `None` means nothing is selected — a real state, not a placeholder:
    /// falling back to the first entry is how an app records the wrong stream.
    selected_source: Option<usize>,
    /// A restored choice waiting for the source list to arrive from the
    /// startup probe.
    pending_source: Option<AudioSource>,
    /// What voxtype reports it can do; empty when `voxtype info` failed.
    engines: Vec<String>,
    models: Vec<String>,
    notices: Notices,
    notices_expanded: bool,
    /// The notice raised by `SourceLost`, so `SourceRecovered` can retract
    /// that one instead of everything the user has not read yet.
    capture_notice: Option<NoticeId>,
    segments: Vec<Segment>,
    provisional: HashMap<Track, Segment>,

    /// Captions only, on an always-on-top window: the mode for watching a
    /// meeting with the transcript over it.
    compact: bool,
    /// The window title as the compositor currently has it, so the title is
    /// only sent when it actually changes — `ViewportCommand::Title` every
    /// frame is a Wayland round trip sixty times a second.
    title_shown: String,

    // Library, read from `fc-store` at startup and after any change.
    conversations: Vec<Conversation>,
    /// Conversations a search found outside the loaded page, listed alongside
    /// it until the search is cleared.
    search_extra: Vec<Conversation>,
    groups: Vec<Group>,
    tags: Vec<Tag>,
    conversation_tags: HashMap<ConversationId, Vec<TagId>>,
    history_segments: HashMap<ConversationId, Vec<Segment>>,

    // Navigation and per-pane transient state
    main_view: MainView,
    /// Set while a deletion is awaiting confirmation.
    pending_delete: Option<PendingDelete>,
    /// A conversation and the offset to scroll to, set by clicking a search
    /// excerpt and consumed by the history view on the next frame it draws.
    pending_scroll: Option<(ConversationId, u64)>,
    sidebar: sidebar::State,
    settings: settings::State,
    export: export_ui::State,
    /// The import running on its own thread, and how many meetings it has
    /// added so far.
    import: Option<library::Running>,

    voxtype_service: ServiceStatus,
    service_monitor: crate::env::ServiceMonitor,
}

impl App {
    /// Builds the app without touching the sound server or the voxtype binary:
    /// the window opens on defaults and [`crate::env::probe_startup`] fills
    /// them in a moment later. Waiting for those subprocesses here cost about
    /// half a second of blank screen at every launch.
    pub fn new(shell_waker: &fastframe_shell::Waker, show_requested: Arc<AtomicBool>) -> Self {
        let store = crate::env::open_store();
        let voxtype = fc_voxtype::cli::find_binary();
        // A config left behind by a process that did not exit cleanly may hold
        // an API key, so it goes before anything else happens.
        crate::env::clear_stale_session_configs();
        let defaults = crate::env::voxtype_defaults();
        // Seeded from voxtype's own configuration, so the settings pane opens
        // showing what voxtype would do unprompted rather than a guess.
        let blank = settings::State::default();
        let settings = settings::State {
            engine: defaults.engine.unwrap_or(blank.engine),
            model: defaults.model.unwrap_or(blank.model),
            language: defaults.language.unwrap_or(blank.language),
            ..settings::State::default()
        };
        let library = store
            .store
            .as_ref()
            .map(|store| crate::env::load_library(store, None))
            .unwrap_or_else(|| crate::env::Probe {
                value: crate::env::Library::default(),
                problem: None,
            });
        if voxtype.is_none() {
            tracing::warn!(
                "voxtype was not found on PATH; recording will fail until it is installed"
            );
        }

        // Every startup problem, not the first one: a machine with no sound
        // server and no voxtype has two things wrong with it, and reporting
        // one of them left the user fixing the wrong thing.
        let mut notices = Notices::default();
        for problem in [store.problem, library.problem].into_iter().flatten() {
            notices.push(NoticeKind::Error, problem);
        }
        if voxtype.is_none() {
            notices.push(NoticeKind::Error, t(readiness::NO_VOXTYPE).to_owned());
        }

        let themes_dir = dirs::config_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("fastcription")
            .join("themes");
        let mut theme_catalog = fastframe_theme::Catalog::default();
        theme_catalog.enable_desktop_themes(fastframe_theme::DesktopThemes {
            slug: "fastcription",
            omarchy_template: fastframe_theme::omarchy::BASE_TEMPLATE,
            omarchy_previous_templates: &[],
            presets: true,
        });

        let tray = {
            let waker = shell_waker.clone();
            fastframe_tray::Tray::spawn(
                fastframe_tray::Config {
                    id: "fastcription",
                    title: "fastcription".to_owned(),
                    icon: chrome::tray_icon_rgba,
                    template_icon: None,
                    themed_icon: false,
                    menu_on_click: false,
                    menu: vec![
                        fastframe_tray::MenuItem::action("show", t("Show fastcription")),
                        fastframe_tray::MenuItem::Separator,
                        fastframe_tray::MenuItem::action("quit", t("Quit")),
                    ],
                },
                move || waker.wake(),
            )
        };

        Self {
            theme_catalog,
            theme_waker: fastframe_theme::Waker::default(),
            themes_dir,
            palette: ThemePalette::base(fastframe_theme::Base::Dark),
            text_rendering: fastframe_text::detect(),
            scrolling: fastframe_scroll::Scrolling::default(),
            tray,
            wants_show: false,
            quit_requested: false,
            show_requested,
            store: store.store,
            library_path: store.path,
            library_read_only: store.read_only,
            captures: session::parec_captures(),
            startup: Some(crate::env::probe_startup(voxtype.clone(), shell_waker)),
            voxtype,
            engine: EngineInfo {
                engine: settings.engine.clone(),
                model: settings.model.clone(),
                language: settings.language.clone(),
                backend: None,
            },
            session: None,
            events: None,
            recording: None,
            live_conversation: None,
            session_config: None,
            state: SessionState::Idle,
            session_start: None,
            accumulated: Duration::ZERO,
            pressure: Pressure::Keeping,
            level_peak: 0.0,
            level_rms: 0.0,
            mic_track: false,
            mic_source: crate::env::default_microphone(),
            sources: Vec::new(),
            selected_source: None,
            pending_source: None,
            engines: Vec::new(),
            models: Vec::new(),
            notices,
            notices_expanded: false,
            capture_notice: None,
            segments: Vec::new(),
            provisional: HashMap::new(),
            compact: false,
            title_shown: String::new(),
            conversations: library.value.conversations,
            search_extra: Vec::new(),
            groups: library.value.groups,
            tags: library.value.tags,
            conversation_tags: library.value.conversation_tags,
            history_segments: HashMap::new(),
            main_view: MainView::Live,
            pending_delete: None,
            pending_scroll: None,
            sidebar: sidebar::State::default(),
            settings,
            export: export_ui::State::default(),
            import: None,
            voxtype_service: ServiceStatus::Unknown,
            service_monitor: crate::env::ServiceMonitor::spawn(),
        }
    }

    /// What survives a restart. Called by eframe on exit and every half
    /// minute; the [`Persisted`] shape is what the next launch reads.
    pub fn save(&mut self, storage: &mut dyn eframe::Storage) {
        let saving = Saving {
            settings: &self.settings,
            // The live choice, or the one still waiting for the source list:
            // quitting before the startup probe lands must not forget it.
            source: self
                .selected_source
                .and_then(|index| self.sources.get(index))
                .or(self.pending_source.as_ref()),
            mic_source: Some(&self.mic_source),
            mic_track: self.mic_track,
        };
        eframe::set_value(storage, PERSISTED_KEY, &saving);
    }

    /// Reads back what [`App::save`] wrote. Runs once, in the first window's
    /// creator: a window reopened from the tray must not have its live state
    /// replaced by whatever was last written to disk.
    pub fn restore(&mut self, storage: &dyn eframe::Storage) {
        let Some(persisted) = eframe::get_value::<Persisted>(storage, PERSISTED_KEY) else {
            return;
        };
        self.settings = persisted.settings;
        self.mic_track = persisted.mic_track;
        if let Some(mic) = persisted.mic_source {
            self.mic_source = mic;
        }
        // Held rather than applied: the live source list has not arrived yet,
        // and an index into a list that does not exist is meaningless.
        self.pending_source = persisted.source;
    }

    /// Tells the user something. Everything the user is told goes through here.
    fn notify(&mut self, kind: NoticeKind, text: impl Into<String>) {
        self.notify_id(kind, text);
    }

    /// As [`App::notify`], returning the notice's id for the one caller that
    /// needs to retract exactly what it raised (`SourceLost`/`SourceRecovered`).
    fn notify_id(&mut self, kind: NoticeKind, text: impl Into<String>) -> NoticeId {
        let text = text.into();
        match kind {
            NoticeKind::Info => tracing::info!(%text, "notice"),
            NoticeKind::Warning => tracing::warn!(%text, "notice"),
            NoticeKind::Error => tracing::error!(%text, "notice"),
        }
        self.notices.push(kind, text)
    }

    /// Takes the window-raise request left by a second launch.
    fn take_show_request(&mut self) -> bool {
        self.show_requested.swap(false, Ordering::SeqCst)
    }
}

/// The name a conversation gets until the user renames it.
fn default_title() -> String {
    let now = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    let format = time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]");
    now.format(&format)
        .map(|stamp| format!("Conversation {stamp}"))
        .unwrap_or_else(|_| "Conversation".to_owned())
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{
        settings, AudioSource, NoticeKind, Notices, Persisted, Saving, INFO_LIFETIME, MAX_NOTICES,
        PERSISTED_KEY,
    };
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    /// eframe's `Storage` over a map, so the persistence tests go through the
    /// same `set_value`/`get_value` — and the same RON — the real one does.
    #[derive(Default)]
    struct MemoryStorage(HashMap<String, String>);

    impl eframe::Storage for MemoryStorage {
        fn get_string(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
        fn set_string(&mut self, key: &str, value: String) {
            self.0.insert(key.to_owned(), value);
        }
        fn remove_string(&mut self, key: &str) {
            self.0.remove(key);
        }
        fn flush(&mut self) {}
    }

    fn sample_settings() -> settings::State {
        settings::State {
            engine: "parakeet".into(),
            model: "parakeet-tdt-0.6b".into(),
            language: "es".into(),
            threads: 6,
            remote_enabled: true,
            remote_endpoint: "http://desktop.lan:8080".into(),
            remote_api_key: "super-secret-token".into(),
            transcript_pt: 30.0,
            ..settings::State::default()
        }
    }

    /// The write and read sides are two types, which only works while their
    /// fields agree. A rename on one of them would silently stop restoring
    /// everything after it — exactly the kind of bug that reads as "settings
    /// sometimes do not stick".
    #[test]
    fn settings_and_the_chosen_source_survive_a_round_trip() {
        let settings = sample_settings();
        let source = AudioSource::sink_input(42, "Zoom", "Zoom meeting audio");
        let mic = crate::env::default_microphone();

        let mut storage = MemoryStorage::default();
        eframe::set_value(
            &mut storage,
            PERSISTED_KEY,
            &Saving {
                settings: &settings,
                source: Some(&source),
                mic_source: Some(&mic),
                mic_track: true,
            },
        );

        let read: Persisted = eframe::get_value(&storage, PERSISTED_KEY).expect("stored blob");
        assert_eq!(read.settings.engine, "parakeet");
        assert_eq!(read.settings.model, "parakeet-tdt-0.6b");
        assert_eq!(read.settings.language, "es");
        assert_eq!(read.settings.threads, 6);
        assert!(read.settings.remote_enabled);
        assert_eq!(read.settings.remote_endpoint, "http://desktop.lan:8080");
        assert_eq!(read.settings.transcript_pt, 30.0);
        assert!(read.mic_track);
        assert_eq!(read.mic_source.as_ref(), Some(&mic));
        // The descriptor, not the index: a sink-input index is stale by the
        // next launch, which is why `reselect` matches on identity.
        let restored = read.source.expect("a stored source");
        assert_eq!(restored.application.as_deref(), Some("Zoom"));
        assert_eq!(restored.kind, source.kind);
    }

    /// egui's storage is a plaintext file in the user's data directory. The
    /// bearer token belongs in the per-session voxtype config, which is created
    /// 0600, and nowhere else.
    #[test]
    fn the_api_key_never_reaches_the_settings_file() {
        let settings = sample_settings();
        let mut storage = MemoryStorage::default();
        eframe::set_value(
            &mut storage,
            PERSISTED_KEY,
            &Saving {
                settings: &settings,
                source: None,
                mic_source: None,
                mic_track: false,
            },
        );

        let stored = eframe::Storage::get_string(&storage, PERSISTED_KEY).expect("stored blob");
        assert!(
            !stored.contains("super-secret-token"),
            "the API key must not be written to disk: {stored}"
        );
        let read: Persisted = eframe::get_value(&storage, PERSISTED_KEY).expect("stored blob");
        assert!(read.settings.remote_api_key.is_empty());
        // Everything else still came back, so the key is skipped rather than
        // the whole blob being refused.
        assert_eq!(read.settings.remote_endpoint, "http://desktop.lan:8080");
    }

    /// A transcriber that fails every pass raises the same text once a second.
    /// Fifty separate entries say nothing a count does not.
    #[test]
    fn the_same_text_in_a_row_collapses_into_a_count() {
        let mut notices = Notices::default();
        let first = notices.push(NoticeKind::Error, "transcribe: engine gone".into());
        let again = notices.push(NoticeKind::Error, "transcribe: engine gone".into());
        assert_eq!(first, again, "a repeat is the same notice, counted up");
        assert_eq!(notices.len(), 1);
        assert_eq!(notices.newest().map(|n| n.count), Some(2));

        notices.push(NoticeKind::Error, "store: disk full".into());
        assert_eq!(notices.len(), 2);
        // Only *consecutive* repeats collapse: the engine failing again after
        // something else went wrong is a new event in the sequence.
        notices.push(NoticeKind::Error, "transcribe: engine gone".into());
        assert_eq!(notices.len(), 3);
    }

    #[test]
    fn the_list_is_capped_and_keeps_the_newest() {
        let mut notices = Notices::default();
        for i in 0..MAX_NOTICES + 10 {
            notices.push(NoticeKind::Warning, format!("problem {i}"));
        }
        assert_eq!(notices.len(), MAX_NOTICES);
        assert_eq!(
            notices.newest().map(|n| n.text.as_str()),
            Some(format!("problem {}", MAX_NOTICES + 9).as_str())
        );
    }

    /// A success dismisses itself; something the user has to act on does not.
    #[test]
    fn info_expires_and_errors_stay() {
        let mut notices = Notices::default();
        notices.push(NoticeKind::Info, "Exported to /tmp/x.txt".into());
        notices.push(NoticeKind::Error, "The library is read-only".into());

        let later = Instant::now() + INFO_LIFETIME + Duration::from_secs(1);
        assert_eq!(notices.expire(later), None, "nothing left to expire");
        assert_eq!(notices.len(), 1);
        assert_eq!(notices.newest().map(|n| n.kind), Some(NoticeKind::Error));
    }

    /// The expiry has to say when to look again, or an auto-dismissing notice
    /// sits on screen until something unrelated causes a repaint.
    #[test]
    fn expiry_reports_when_the_next_one_is_due() {
        let mut notices = Notices::default();
        notices.push(NoticeKind::Info, "Imported 3 meetings".into());
        let due = notices.expire(Instant::now()).expect("one info notice");
        assert!(
            due <= INFO_LIFETIME && due > Duration::ZERO,
            "due in {due:?}"
        );
    }

    /// `SourceRecovered` means the capture came back, and nothing else.
    #[test]
    fn retracting_one_notice_leaves_the_others() {
        let mut notices = Notices::default();
        let kept = notices.push(NoticeKind::Error, "voxtype was not found".into());
        let lost = notices.push(NoticeKind::Warning, "Audio source lost: gone".into());
        notices.remove(lost);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices.newest().map(|n| n.id), Some(kept));
    }
}
