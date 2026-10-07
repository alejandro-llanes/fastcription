//! Everything the app needs from the machine it runs on: the library database,
//! the list of capture sources, what voxtype is configured to do, and whether
//! its service is up.
//!
//! Each probe fails on its own and says so, because the useful states are
//! partial. voxtype missing is not a reason to hide a past transcript; a
//! suspended sound server is not a reason to refuse to open.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use fc_core::{AudioSource, Conversation, ConversationId, EngineInfo, Group, Segment, SourceKind, Tag, TagId};
use fc_store::{ConversationFilter, Store};
use std::collections::HashMap;

use crate::session::SharedStore;

/// How many conversations the sidebar loads at once. The list is a navigation
/// aid, not an archive browser; paging past this is a feature for later.
const LIBRARY_PAGE: u32 = 200;

/// The microphone, when the user opts into the second track.
///
/// `@DEFAULT_SOURCE@` is PulseAudio's own alias for whatever the user has set
/// as their input, which is the only defensible default: picking the first
/// device found would silently record the wrong microphone on any machine with
/// more than one.
pub fn default_microphone() -> AudioSource {
    AudioSource::named(
        SourceKind::Device,
        "@DEFAULT_SOURCE@",
        "Default microphone",
    )
}

pub struct Probe<T> {
    pub value: T,
    /// Set when the probe failed or partially failed; shown to the user as-is.
    pub problem: Option<String>,
}

impl<T> Probe<T> {
    fn ok(value: T) -> Self {
        Self {
            value,
            problem: None,
        }
    }
}

/// Opens the library. A failure here disables recording but leaves the app
/// usable, so it returns `None` rather than aborting startup.
pub fn open_store() -> Probe<Option<SharedStore>> {
    match Store::open_default() {
        Ok(store) => {
            // Any conversation still marked active belongs to a process that
            // died mid-recording. Its committed segments are intact.
            match store.reap_active() {
                Ok(0) => {}
                Ok(n) => tracing::warn!(count = n, "marked interrupted conversations from a previous run"),
                Err(err) => tracing::warn!(%err, "could not reap interrupted conversations"),
            }
            Probe::ok(Some(Arc::new(Mutex::new(store))))
        }
        Err(err) => Probe {
            value: None,
            problem: Some(format!(
                "The conversation library could not be opened, so recording is disabled: {err}"
            )),
        },
    }
}

/// Capture targets, newest enumeration wins. An empty list with no problem
/// reported means the sound server genuinely offers nothing to record.
pub fn list_sources() -> Probe<Vec<AudioSource>> {
    match fc_audio::enumerate() {
        Ok(sources) => Probe::ok(sources),
        Err(err) => Probe {
            value: Vec::new(),
            problem: Some(format!("No audio sources could be listed: {err}")),
        },
    }
}

/// What voxtype will actually do, recorded with each conversation.
///
/// The daemon's status knows the model and the acceleration backend but not the
/// engine or language, and the CLI adapter deliberately never reads the user's
/// config (decision D6), so the two sources are combined here.
pub fn probe_engine(binary: Option<&PathBuf>) -> EngineInfo {
    let defaults = fc_voxtype::config::default_path()
        .and_then(|path| fc_voxtype::config::read_defaults(path).ok())
        .unwrap_or_default();
    let engine = defaults.engine.unwrap_or_else(|| "whisper".to_owned());
    let language = defaults.language.unwrap_or_else(|| "en".to_owned());

    if let Some(binary) = binary {
        if let Ok(status) = fc_voxtype::cli::status(binary) {
            return status.engine_info(engine, language);
        }
    }

    EngineInfo {
        engine,
        model: defaults.model.unwrap_or_else(|| "unknown".to_owned()),
        language,
        backend: None,
    }
}

/// Maps the service's systemd state onto the three states the pill shows.
pub fn service_status() -> crate::app::ServiceStatus {
    use crate::app::ServiceStatus as Pill;
    match fc_voxtype::service::status() {
        Ok(fc_voxtype::service::ServiceStatus::Found { active_state, .. }) => {
            if active_state == "active" {
                Pill::Running
            } else {
                Pill::Stopped
            }
        }
        Ok(fc_voxtype::service::ServiceStatus::NotInstalled) | Err(_) => Pill::Unknown,
    }
}

/// The library rows the sidebar and history panes render.
#[derive(Default)]
pub struct Library {
    pub conversations: Vec<Conversation>,
    pub groups: Vec<Group>,
    pub tags: Vec<Tag>,
    pub conversation_tags: HashMap<ConversationId, Vec<TagId>>,
}

/// Reads the library for display.
///
/// The views want whole [`Conversation`]s — source, engine and status included —
/// which `list_conversations` does not carry, so each listed row is fetched in
/// full. That is one query per row: fine for a page of 200, and the thing to
/// revisit when the sidebar learns to render summaries directly.
pub fn load_library(store: &SharedStore) -> Probe<Library> {
    let guard = match store.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };

    let filter = ConversationFilter {
        limit: Some(LIBRARY_PAGE),
        ..Default::default()
    };

    let summaries = match guard.list_conversations(&filter) {
        Ok(rows) => rows,
        Err(err) => {
            return Probe {
                value: Library::default(),
                problem: Some(format!("The conversation list could not be read: {err}")),
            }
        }
    };

    let mut library = Library {
        groups: guard.list_groups().unwrap_or_default(),
        tags: guard.list_tags().unwrap_or_default(),
        ..Default::default()
    };

    for summary in summaries {
        library
            .conversation_tags
            .insert(summary.id, summary.tags.iter().map(|t| t.id).collect());
        match guard.get_conversation(summary.id) {
            Ok(conversation) => library.conversations.push(conversation),
            Err(err) => tracing::warn!(id = %summary.id, %err, "skipping unreadable conversation"),
        }
    }

    Probe::ok(library)
}

pub fn load_segments(store: &SharedStore, id: ConversationId) -> Vec<Segment> {
    let guard = match store.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    match guard.load_segments(id) {
        Ok(segments) => segments,
        Err(err) => {
            tracing::warn!(id = %id, %err, "could not load segments");
            Vec::new()
        }
    }
}
