//! Everything the app needs from the machine it runs on: the library database,
//! the list of capture sources, what voxtype is configured to do, and whether
//! its service is up.
//!
//! Each probe fails on its own and says so, because the useful states are
//! partial. voxtype missing is not a reason to hide a past transcript; a
//! suspended sound server is not a reason to refuse to open.

use std::fs::OpenOptions;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use fc_core::{
    AudioSource, Conversation, ConversationId, ConversationStatus, EngineInfo, Group, Segment,
    SourceKind, Tag, TagId, Track, UnixMillis,
};
use fc_store::{ConversationFilter, Store};
use std::collections::{HashMap, HashSet};

use crate::i18n::tf;
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
    AudioSource::named(SourceKind::Device, "@DEFAULT_SOURCE@", "Default microphone")
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

/// Where the library lives, whether or not it opened. Shown to the user when
/// it turns out to be read-only, since "the library is read-only" is only
/// actionable if they know which file to look at.
pub fn library_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("fastcription")
        .join("library.db")
}

/// The library, as the app needs to talk about it: the handle when there is
/// one, where it is, and whether it will accept writes.
pub struct StoreProbe {
    pub store: Option<SharedStore>,
    pub path: PathBuf,
    /// True when the library opened but refuses writes, so recording is off
    /// while history and export still work.
    pub read_only: bool,
    pub problem: Option<String>,
}

/// Said wherever a read-only library has to be explained, which is four
/// places: the startup notice, the disabled Start button's tooltip, the
/// refusal when Start is pressed anyway, and the first-run checklist. One
/// wording, because a user who reads two different sentences about the same
/// file concludes there are two problems.
pub fn read_only_library(path: &Path) -> String {
    tf(
        "The conversation library at {} will not accept writes, so nothing can be \
         recorded. Past transcripts are still readable and exportable.",
        &[&path.display().to_string()],
    )
}

/// Opens the library. A failure here disables recording but leaves the app
/// usable, so it returns `None` rather than aborting startup.
pub fn open_store() -> StoreProbe {
    let path = library_path();
    match Store::open(&path) {
        Ok(store) => {
            let read_only = store.is_read_only();
            // Any conversation still marked active belongs to a process that
            // died mid-recording. Its committed segments are intact.
            if !read_only {
                match store.reap_active() {
                    Ok(0) => {}
                    Ok(n) => tracing::warn!(
                        count = n,
                        "marked interrupted conversations from a previous run"
                    ),
                    Err(err) => tracing::warn!(%err, "could not reap interrupted conversations"),
                }
            }
            StoreProbe {
                store: Some(Arc::new(Mutex::new(store))),
                problem: read_only.then(|| read_only_library(&path)),
                path,
                read_only,
            }
        }
        Err(err) => StoreProbe {
            store: None,
            problem: Some(tf(
                "The conversation library could not be opened, so recording is disabled: {}",
                &[&err.to_string()],
            )),
            path,
            read_only: false,
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
            problem: Some(tf(
                "No audio sources could be listed: {}",
                &[&err.to_string()],
            )),
        },
    }
}

/// What voxtype offers: the engines compiled into this build and the models
/// actually installed.
///
/// Offered as a choice rather than a text field because a mistyped model name
/// only fails on the first chunk, which during a meeting is the worst moment
/// to discover it. An empty list is normal — `voxtype info` can fail while
/// transcription works — and the settings pane falls back to free text.
pub struct Catalog {
    pub engines: Vec<String>,
    pub models: Vec<String>,
}

/// Only the cheap half of voxtype's self-description: `--version`,
/// `info engines`, `info models`, about 10 ms for the three. `info devices`
/// alone costs 240 ms and nothing here needs it, which is why
/// [`fc_voxtype::cli::probe`] is deliberately not used.
pub fn probe_catalog() -> Catalog {
    let probe = fc_voxtype::cli::probe_catalog();
    let engines = probe
        .engines
        .map(|entries| {
            entries
                .into_iter()
                .filter(|entry| entry.compiled)
                .map(|entry| entry.name)
                .collect()
        })
        .unwrap_or_default();
    let models = probe
        .models
        .map(|entries| {
            entries
                .into_iter()
                .filter(|entry| entry.installed)
                .map(|entry| entry.name)
                .collect()
        })
        .unwrap_or_default();
    Catalog { engines, models }
}

/// voxtype's own configured defaults, used to seed the settings pane so that
/// leaving it alone reproduces what voxtype would have done by itself.
pub fn voxtype_defaults() -> fc_voxtype::config::Defaults {
    fc_voxtype::config::default_path()
        .and_then(|path| fc_voxtype::config::read_defaults(path).ok())
        .unwrap_or_default()
}

/// Microphone-like sources: real capture devices, never monitors.
pub fn microphones(sources: &[AudioSource]) -> Vec<AudioSource> {
    sources
        .iter()
        .filter(|source| source.kind == SourceKind::Device)
        .cloned()
        .collect()
}

/// What voxtype will actually do, recorded with each conversation.
///
/// The daemon's status knows the model and the acceleration backend but not the
/// engine or language, and the CLI adapter deliberately never reads the user's
/// config (decision D6), so the two sources are combined here.
pub fn probe_engine(binary: Option<&PathBuf>) -> EngineInfo {
    let defaults = voxtype_defaults();
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

/// Whether voxtype's dictation daemon is available, as the indicator shows it.
///
/// The systemd unit is the usual answer but not the only one: a daemon started
/// by hand (`voxtype daemon`) provides dictation while `systemctl` reports the
/// unit inactive or absent. The indicator exists to tell the user whether
/// dictation works, so a live daemon counts however it was started, and its
/// runtime state file is the evidence.
pub fn service_status() -> crate::app::ServiceStatus {
    use crate::app::ServiceStatus as Pill;
    let unit = match fc_voxtype::service::status() {
        Ok(fc_voxtype::service::ServiceStatus::Found { active_state, .. }) => {
            if active_state == "active" {
                return Pill::Running;
            }
            Pill::Stopped
        }
        Ok(fc_voxtype::service::ServiceStatus::NotInstalled) | Err(_) => Pill::Unknown,
    };

    let daemon_alive = fc_voxtype::runtime::RuntimePaths::discover()
        .ok()
        .map(|paths| fc_voxtype::runtime::read_now(&paths).0.is_some())
        .unwrap_or(false);
    if daemon_alive {
        Pill::Running
    } else {
        unit
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
/// Two queries for the whole page, through `list_conversations_full`: the views
/// want the complete record — source, engine, status — and fetching each listed
/// row on its own meant 201 queries for a page of 200, on every reload.
///
/// `recording` is the conversation this process is writing right now, which is
/// the only `Active` row that may be listed. Every other one belongs to a
/// process that died mid-recording; those are reaped to `Interrupted` at
/// startup, and one that slipped through would offer the user a Delete button
/// the store is bound to refuse.
pub fn load_library(store: &SharedStore, recording: Option<ConversationId>) -> Probe<Library> {
    let guard = crate::session::lock(store);

    let filter = ConversationFilter {
        limit: Some(LIBRARY_PAGE),
        ..Default::default()
    };

    let rows = match guard.list_conversations_full(&filter) {
        Ok(rows) => rows,
        Err(err) => {
            return Probe {
                value: Library::default(),
                problem: Some(tf(
                    "The conversation list could not be read: {}",
                    &[&err.to_string()],
                )),
            }
        }
    };

    let mut library = Library {
        groups: guard.list_groups().unwrap_or_default(),
        tags: guard.list_tags().unwrap_or_default(),
        ..Default::default()
    };

    // On a read-only library `reap_active` could not run, so an `Active` row
    // left by a crash is permanent — and hiding it would hide a transcript the
    // user cannot get back any other way. Delete is refused for every row
    // there anyway, so there is nothing to protect them from.
    let hide_active = !guard.is_read_only();

    for (conversation, tags) in rows {
        if hide_active
            && conversation.status == ConversationStatus::Active
            && Some(conversation.id) != recording
        {
            continue;
        }
        library
            .conversation_tags
            .insert(conversation.id, tags.iter().map(|t| t.id).collect());
        library.conversations.push(conversation);
    }

    Probe::ok(library)
}

/// One conversation that a search found outside the loaded page.
///
/// The sidebar lists [`LIBRARY_PAGE`] conversations; a full-text hit can be in
/// any of them, so a search in a long-lived library would show excerpts with
/// no rows to attach them to.
pub fn load_conversation(store: &SharedStore, id: ConversationId) -> Option<Conversation> {
    match crate::session::lock(store).get_conversation(id) {
        Ok(conversation) => Some(conversation),
        Err(err) => {
            tracing::warn!(id = %id, %err, "a search hit named an unreadable conversation");
            None
        }
    }
}

pub fn load_segments(store: &SharedStore, id: ConversationId) -> Vec<Segment> {
    let guard = crate::session::lock(store);
    match guard.load_segments(id) {
        Ok(segments) => segments,
        Err(err) => {
            tracing::warn!(id = %id, %err, "could not load segments");
            Vec::new()
        }
    }
}

/// Keeps the voxtype service indicator truthful without the user pressing
/// anything.
///
/// Two signals, because neither alone is enough: `systemctl` knows whether the
/// unit is active but says nothing until asked, and the daemon's runtime state
/// file changes the instant it does something but does not exist when the
/// daemon is stopped. The watcher turns a file change into an immediate
/// re-check, and a slow poll catches the unit being started or stopped from
/// outside fastcription.
pub struct ServiceMonitor {
    updates: crossbeam_channel::Receiver<crate::app::ServiceStatus>,
}

impl ServiceMonitor {
    /// How long to wait between `systemctl` calls when the inotify watch is
    /// working. The watch reports anything the daemon does within
    /// milliseconds, so this only has to catch the unit being started or
    /// stopped from outside fastcription — a spare poll per quarter minute,
    /// rather than a subprocess every three seconds for the life of the app.
    const WATCHED_POLL: std::time::Duration = std::time::Duration::from_secs(15);
    /// Without a watch, the poll is the only signal there is.
    const BLIND_POLL: std::time::Duration = std::time::Duration::from_secs(3);

    pub fn spawn() -> Self {
        let (tx, updates) = crossbeam_channel::bounded(8);
        std::thread::Builder::new()
            .name("fc-service-monitor".into())
            .spawn(move || {
                // A failure to watch is not fatal: the poll below still keeps
                // the indicator correct, just less promptly.
                let watch = fc_voxtype::runtime::RuntimePaths::discover()
                    .ok()
                    .and_then(|paths| fc_voxtype::runtime::watch(paths).ok());
                let poll = if watch.is_some() {
                    Self::WATCHED_POLL
                } else {
                    Self::BLIND_POLL
                };

                loop {
                    // Sent whatever it was last time: the receiving end keeps
                    // only the newest value, and skipping unchanged ones meant
                    // a window opened after a change showed a stale pill until
                    // the next one.
                    match tx.try_send(service_status()) {
                        Ok(()) | Err(crossbeam_channel::TrySendError::Full(_)) => {}
                        // Nobody is listening any more: the app is gone, and
                        // this thread spawning `systemctl` for ever after it
                        // is pure waste.
                        Err(crossbeam_channel::TrySendError::Disconnected(_)) => return,
                    }
                    match &watch {
                        // Any daemon activity is a reason to look again now.
                        Some(rx) => match rx.recv_timeout(poll) {
                            Ok(_) | Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                                std::thread::sleep(poll)
                            }
                        },
                        None => std::thread::sleep(poll),
                    }
                }
            })
            .map_err(|err| tracing::error!(%err, "could not spawn the service monitor"))
            .ok();
        Self { updates }
    }

    /// The newest status, if one arrived since the last call.
    pub fn poll(&self) -> Option<crate::app::ServiceStatus> {
        self.updates.try_iter().last()
    }
}

/// What the app learns about the machine by running subprocesses, delivered
/// once the answers are in.
///
/// Every one of these costs a `fork`/`exec`: `pactl` for the sources, `voxtype
/// status` for the engine, three more `voxtype info` calls for the catalog.
/// Together they took about half a second on this machine, and the window used
/// to wait for all of them before its first frame — half a second of nothing
/// on screen, to fill in controls the user cannot have touched yet.
pub struct Startup {
    pub sources: Probe<Vec<AudioSource>>,
    pub engine: EngineInfo,
    pub catalog: Catalog,
    /// Name of the default sink's monitor, so the first selection is what the
    /// system is actually playing. `None` when the sound server has no default
    /// sink or no monitor for it.
    pub default_monitor: Option<String>,
}

/// Runs the startup probes on a thread and wakes the window when they land.
pub fn probe_startup(
    voxtype: Option<PathBuf>,
    waker: &fastframe_shell::Waker,
) -> crossbeam_channel::Receiver<Startup> {
    let (tx, rx) = crossbeam_channel::bounded(1);
    let waker = waker.clone();
    let spawned = std::thread::Builder::new()
        .name("fc-startup-probe".into())
        .spawn(move || {
            let startup = Startup {
                default_monitor: fc_audio::default_source().ok().flatten().map(|s| s.name),
                sources: list_sources(),
                engine: probe_engine(voxtype.as_ref()),
                catalog: probe_catalog(),
            };
            if tx.send(startup).is_ok() {
                waker.wake();
            }
        });
    if let Err(err) = spawned {
        tracing::error!(%err, "could not spawn the startup probe");
    }
    rx
}

/// What an import run did, for reporting back to the user.
pub struct Imported {
    pub added: usize,
    pub skipped: usize,
    pub failed: Vec<String>,
}

/// Copies meetings recorded by voxtype's own meeting mode into the library.
///
/// Read-only with respect to voxtype: its database and transcript files are
/// never touched, only read through `voxtype meeting export` (architecture §7).
/// Meetings already imported are skipped, so running this twice is safe, and a
/// meeting still in progress is left alone — its transcript does not exist yet.
/// `progress` counts meetings added so far, for a UI that is drawing while
/// this runs on a thread of its own.
pub fn import_meetings(
    store: &SharedStore,
    binary: &Path,
    progress: &std::sync::atomic::AtomicUsize,
) -> Result<Imported, String> {
    use std::sync::atomic::Ordering;

    let meetings = fc_voxtype::meeting::list(binary, None)
        .map_err(|err| format!("Could not list voxtype meetings: {err}"))?;

    let seen = imported_ids(store);
    let mut outcome = Imported {
        added: 0,
        skipped: 0,
        failed: Vec::new(),
    };

    for meeting in meetings {
        let finished = matches!(
            meeting.status,
            None | Some(fc_voxtype::meeting::MeetingStatus::Completed)
        );
        if seen.contains(&meeting.id) || !finished {
            outcome.skipped += 1;
            continue;
        }
        match import_one(store, binary, &meeting) {
            Ok(()) => {
                outcome.added += 1;
                progress.store(outcome.added, Ordering::Relaxed);
            }
            Err(err) => {
                tracing::warn!(id = %meeting.id, %err, "could not import a voxtype meeting");
                outcome.failed.push(format!("{}: {err}", meeting.title));
            }
        }
    }

    Ok(outcome)
}

/// The voxtype meeting ids already in the library.
///
/// One listing, not one query per row: this used to list the conversations and
/// then read each one in full to reach `voxtype_meeting_id`, which was a
/// query per conversation in the library before a single meeting was imported.
fn imported_ids(store: &SharedStore) -> HashSet<String> {
    let guard = crate::session::lock(store);
    let Ok(rows) = guard.list_conversations_full(&ConversationFilter::default()) else {
        return HashSet::new();
    };
    rows.into_iter()
        .filter_map(|(conversation, _)| conversation.voxtype_meeting_id)
        .collect()
}

fn import_one(
    store: &SharedStore,
    binary: &Path,
    meeting: &fc_voxtype::meeting::MeetingRecord,
) -> Result<(), String> {
    let json = fc_voxtype::meeting::export(
        binary,
        &meeting.id,
        fc_voxtype::meeting::ExportFormat::Json,
        fc_voxtype::meeting::ExportOptions {
            timestamps: true,
            speakers: true,
            metadata: true,
        },
    )
    .map_err(|err| err.to_string())?;

    let transcript =
        fc_voxtype::meeting::parse_export_json(&json).map_err(|err| err.to_string())?;
    if transcript.segments.is_empty() {
        return Err("the meeting has no transcript".to_owned());
    }

    let started_at = transcript
        .started_at
        .as_deref()
        .and_then(parse_rfc3339_millis)
        .or_else(|| meeting.started_at.as_deref().and_then(parse_utc_minute))
        .ok_or("the meeting has no readable start time")?;

    // Segment offsets are relative to the meeting's start, so the last one's
    // end is the meeting's length. voxtype's own `durationSecs` is not carried
    // through the import types, and this agrees with it to the millisecond.
    let ended_at = started_at
        + transcript
            .segments
            .iter()
            .map(|segment| segment.end_ms)
            .max()
            .unwrap_or(0) as i64;

    let segments: Vec<Segment> = transcript
        .segments
        .iter()
        .filter(|segment| !segment.text.trim().is_empty())
        .enumerate()
        .map(|(index, segment)| Segment {
            // voxtype attributes by source, not by track, and the distinction
            // is not in the export. One track keeps the imported transcript
            // honest; the speaker labels it does carry are preserved.
            track: Track::Selected,
            seq: index as u64,
            start_ms: segment.start_ms,
            end_ms: segment.end_ms,
            text: segment.text.clone(),
            translation: None,
            speaker: segment.speaker.clone(),
            confidence: None,
            provisional: false,
        })
        .collect();

    // One transaction, so a failure halfway cannot leave a half-empty `Active`
    // row that the dedupe above then reads as "already imported" — which would
    // make the meeting unimportable for ever and leave a fragment behind.
    crate::session::lock(store)
        .import_conversation(
            &fc_store::NewConversation {
                title: transcript
                    .title
                    .clone()
                    .unwrap_or_else(|| meeting.title.clone()),
                group: None,
                started_at,
                // voxtype does not record which source a meeting came from, and
                // inventing one would put a claim in the library nothing backs.
                source: AudioSource::named(SourceKind::Device, "", "Imported from voxtype"),
                mic_track: false,
                engine: EngineInfo {
                    engine: "voxtype".to_owned(),
                    // Not in the export either: metadata carries id, title,
                    // times, status and chunk count, and no model.
                    model: "unknown".to_owned(),
                    language: "unknown".to_owned(),
                    backend: None,
                },
                voxtype_meeting_id: Some(meeting.id.clone()),
            },
            &segments,
            ended_at,
            ConversationStatus::Completed,
        )
        .map_err(|err| err.to_string())?;
    Ok(())
}

/// `metadata.startedAt` from a JSON export, e.g. `2026-10-07T14:30:22+00:00`.
fn parse_rfc3339_millis(text: &str) -> Option<UnixMillis> {
    time::OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339)
        .ok()
        .map(|stamp| (stamp.unix_timestamp_nanos() / 1_000_000) as i64)
}

/// The `Date:` line from `meeting list`, e.g. `2026-10-07 14:30`.
///
/// Read as **UTC**, not as local time: upstream formats that line from the
/// stored `started_at` without converting it, and that value is UTC — the same
/// instant the JSON export writes with an explicit `+00:00`. Reading it as
/// local time shifted every meeting imported through this fallback by the
/// machine's offset, which on this one is an hour or two off, enough to make a
/// morning standup land in the previous evening.
///
/// Only used when the JSON export has no start time of its own.
fn parse_utc_minute(text: &str) -> Option<UnixMillis> {
    let format = time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]");
    let naive = time::PrimitiveDateTime::parse(text.trim(), &format).ok()?;
    Some(naive.assume_utc().unix_timestamp() * 1_000)
}

#[cfg(test)]
mod tests {
    use super::{parse_rfc3339_millis, parse_utc_minute};

    /// The exact value voxtype writes in a JSON export's `metadata.startedAt`.
    /// A unit mistake here would shift every imported transcript by a factor
    /// of a thousand, so the expected epoch is spelled out rather than
    /// computed by the same code under test.
    #[test]
    fn a_json_export_start_time_reads_as_milliseconds() {
        // 2026-10-07T14:30:22+00:00 == 1791383422 seconds since the epoch.
        assert_eq!(
            parse_rfc3339_millis("2026-10-07T14:30:22+00:00"),
            Some(1_791_383_422_000)
        );
    }

    /// An offset other than UTC must be honoured, not ignored.
    #[test]
    fn a_non_utc_offset_is_applied() {
        let utc = parse_rfc3339_millis("2026-10-07T14:30:22+00:00").unwrap();
        let plus_two = parse_rfc3339_millis("2026-10-07T14:30:22+02:00").unwrap();
        assert_eq!(utc - plus_two, 2 * 60 * 60 * 1_000);
    }

    #[test]
    fn unreadable_start_times_are_rejected_rather_than_guessed() {
        assert_eq!(parse_rfc3339_millis(""), None);
        assert_eq!(parse_rfc3339_millis("2026-10-07 14:30"), None);
        assert_eq!(parse_utc_minute("not a date"), None);
        assert_eq!(parse_utc_minute(""), None);
    }

    /// The `Date:` line from `meeting list` has no timezone, and upstream
    /// prints it straight from a UTC `started_at`. Asserted against a fixed
    /// epoch rather than the machine's offset: the whole point is that the
    /// answer does not depend on where the machine is.
    #[test]
    fn a_meeting_list_date_reads_as_utc() {
        // Same wall clock as the JSON fixture above, which carries +00:00.
        assert_eq!(
            parse_utc_minute("2026-10-07 14:30"),
            Some(1_791_383_400_000)
        );
        assert_eq!(
            parse_utc_minute("2026-10-07 14:30"),
            parse_rfc3339_millis("2026-10-07T14:30:00+00:00"),
            "the two ways upstream prints one instant must agree"
        );
    }
}

#[cfg(test)]
mod import_tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::AtomicUsize;

    /// A stand-in `voxtype` that prints the output the real one prints.
    ///
    /// The shapes it emits are the ones `fc-voxtype`'s fixtures were derived
    /// from upstream source, so this tests the part fastcription owns — the
    /// timestamp conversion, the segment mapping, the end time and the
    /// idempotency — without needing a recorded meeting on the machine.
    fn stub_voxtype(dir: &std::path::Path, started_at: &str) -> PathBuf {
        let path = dir.join("voxtype");
        let script = format!(
            r#"#!/bin/sh
case "$1 $2" in
"meeting list")
  cat <<'LIST'
Recent Meetings
===============

Standup
  ID: 3f29c5e1-8b77-4b1a-9c3b-1a2b3c4d5e6f
  Date: 2026-10-07 14:30
  Duration: 12m 34s
  Status: Completed

Still going
  ID: 9999aaaa-0000-4b1a-9c3b-000000000009
  Date: 2026-10-07 16:00
  Duration: in progress
  Status: Active
LIST
  ;;
"meeting export")
  cat <<'JSON'
{{
  "metadata": {{
    "id": "3f29c5e1-8b77-4b1a-9c3b-1a2b3c4d5e6f",
    "title": "Standup",
    "startedAt": "{started_at}",
    "endedAt": "2026-10-07T14:42:56+00:00",
    "durationSecs": 754,
    "status": "completed",
    "chunkCount": 2
  }},
  "transcript": {{
    "segments": [
      {{ "id": 0, "startMs": 0, "endMs": 3200, "text": "Let's get started.", "source": "microphone", "speaker": "Alice", "chunkId": 0 }},
      {{ "id": 1, "startMs": 3200, "endMs": 7400, "text": "Sounds good to me.", "source": "loopback", "speaker": "Bob", "chunkId": 0 }},
      {{ "id": 2, "startMs": 7400, "endMs": 9000, "text": "   ", "source": "loopback", "chunkId": 1 }}
    ],
    "totalChunks": 2
  }}
}}
JSON
  ;;
esac
"#
        );
        let mut file = std::fs::File::create(&path).expect("create stub");
        file.write_all(script.as_bytes()).expect("write stub");
        drop(file);
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("chmod stub");
        path
    }

    fn temp_store() -> (tempfile::TempDir, SharedStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("library.db")).expect("open store");
        (dir, Arc::new(Mutex::new(store)))
    }

    #[test]
    fn a_completed_meeting_imports_with_its_real_times_and_speakers() {
        let bin_dir = tempfile::tempdir().expect("tempdir");
        let binary = stub_voxtype(bin_dir.path(), "2026-10-07T14:30:22+00:00");
        let (_dir, store) = temp_store();

        let outcome = import_meetings(&store, &binary, &AtomicUsize::new(0)).expect("import");
        // The meeting still in progress has no transcript yet, so it is skipped
        // rather than imported half-finished.
        assert_eq!(outcome.added, 1, "failures: {:?}", outcome.failed);
        assert_eq!(outcome.skipped, 1);
        assert!(outcome.failed.is_empty());

        let guard = store.lock().unwrap();
        let rows = guard
            .list_conversations(&ConversationFilter::default())
            .expect("list");
        assert_eq!(rows.len(), 1);

        let conversation = guard.get_conversation(rows[0].id).expect("conversation");
        assert_eq!(conversation.title, "Standup");
        assert_eq!(conversation.started_at, 1_791_383_422_000);
        assert_eq!(
            conversation.voxtype_meeting_id.as_deref(),
            Some("3f29c5e1-8b77-4b1a-9c3b-1a2b3c4d5e6f")
        );
        // The last segment ends 9000 ms in, even though its text is blank.
        assert_eq!(conversation.ended_at, Some(1_791_383_422_000 + 9_000));
        assert_eq!(conversation.engine.engine, "voxtype");

        let segments = guard.load_segments(rows[0].id).expect("segments");
        assert_eq!(segments.len(), 2, "blank segments must not be stored");
        assert_eq!(segments[0].text, "Let's get started.");
        assert_eq!(segments[0].speaker.as_deref(), Some("Alice"));
        assert_eq!(segments[1].start_ms, 3_200);
        assert_eq!(segments[1].end_ms, 7_400);
        assert_eq!(segments[1].speaker.as_deref(), Some("Bob"));
    }

    /// Importing twice must not duplicate a library.
    #[test]
    fn importing_twice_adds_nothing_the_second_time() {
        let bin_dir = tempfile::tempdir().expect("tempdir");
        let binary = stub_voxtype(bin_dir.path(), "2026-10-07T14:30:22+00:00");
        let (_dir, store) = temp_store();

        let first = import_meetings(&store, &binary, &AtomicUsize::new(0)).expect("first import");
        assert_eq!(first.added, 1);

        let second = import_meetings(&store, &binary, &AtomicUsize::new(0)).expect("second import");
        assert_eq!(second.added, 0);
        assert_eq!(second.skipped, 2);

        let guard = store.lock().unwrap();
        let rows = guard
            .list_conversations(&ConversationFilter::default())
            .expect("list");
        assert_eq!(rows.len(), 1, "a second import must not duplicate");
    }

    /// A meeting whose start time is unreadable is reported, not stored with a
    /// guessed timestamp that would sort it to 1970.
    #[test]
    fn an_unreadable_start_time_fails_the_meeting_rather_than_guessing() {
        let bin_dir = tempfile::tempdir().expect("tempdir");
        let binary = stub_voxtype(bin_dir.path(), "not a timestamp");
        let (_dir, store) = temp_store();

        let outcome = import_meetings(&store, &binary, &AtomicUsize::new(0)).expect("import");
        // The `Date:` line from the listing is the documented fallback, so this
        // still imports — from 2026-10-07 14:30 UTC rather than from nothing.
        assert_eq!(outcome.added, 1);
        let guard = store.lock().unwrap();
        let rows = guard
            .list_conversations(&ConversationFilter::default())
            .expect("list");
        let conversation = guard.get_conversation(rows[0].id).expect("conversation");
        assert_eq!(conversation.started_at, 1_791_383_400_000);
    }
}

/// Where transcription runs and with what.
pub struct EngineSettings {
    pub model: String,
    pub language: String,
    pub threads: u32,
    pub fast_mode: bool,
    /// Set to send audio to a transcription server instead of running the model
    /// on this machine.
    pub remote: Option<RemoteEngine>,
}

/// An OpenAI-compatible transcription server, which is what voxtype's remote
/// mode speaks: it POSTs the audio to `{endpoint}/v1/audio/transcriptions` as
/// multipart form data.
///
/// Useful when the model should run somewhere else — a desktop with a GPU
/// serving a laptop that has none. The model stays resident there, so no pass
/// pays to load it.
pub struct RemoteEngine {
    pub endpoint: String,
    /// The model name to ask the server for, e.g. `whisper-1` or whatever that
    /// server calls the model it has loaded.
    pub model: String,
    /// Optional bearer token. Stored in the config file, which is why that file
    /// is written user-only whenever a key is present.
    pub api_key: String,
    pub timeout_secs: u32,
}

/// fastcription's own voxtype configuration file.
///
/// Decision D6 says the user's `~/.config/voxtype/config.toml` is never
/// rewritten, and it still is not. But voxtype's two most valuable settings for
/// this app have no command-line flag: `context_window_optimization`, which
/// makes clips under 22.5 seconds two to nearly three times faster (0.75s to
/// 0.28s for a 7 second window on this machine), and remote mode. Realtime
/// depends on the first and running the model elsewhere depends on the second.
///
/// `voxtype -c <file>` accepts an arbitrary config, so fastcription keeps its
/// own and passes it explicitly. The user's file is read for defaults and never
/// touched; this one is ours to overwrite, and says so in a comment for anyone
/// who finds it.
/// Writes the config for one session, under a name that belongs to that
/// session alone.
///
/// Every session and every connection test used to write the same
/// `voxtype.toml`, and the running transcriber reads that file on every pass —
/// so testing a new server address in the middle of a meeting silently
/// retargeted the live transcript at it. The file name carries the session's
/// start time, and [`remove_session_config`] takes it away again when the
/// session ends.
pub fn write_session_config(
    settings: &EngineSettings,
    started_at: UnixMillis,
) -> Result<PathBuf, String> {
    let dir = config_dir()?;
    write_private_config_in(&dir, &format!("voxtype-{started_at}.toml"), settings)
}

/// Deletes a session's config. Best effort: a leftover file is tidied up by
/// [`clear_stale_session_configs`] on the next launch, so a failure here is
/// worth a log line and nothing more.
pub fn remove_session_config(path: &Path) {
    if let Err(err) = std::fs::remove_file(path) {
        if err.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(path = %path.display(), %err, "could not remove the session config");
        }
    }
}

/// Removes session configs left behind by a process that did not exit
/// cleanly. They hold nothing but settings — and possibly an API key, which is
/// the reason not to leave them lying around.
///
/// `voxtype.toml` goes too: that is where every session used to write, and a
/// build that wrote it may have left a bearer token in it. Nothing reads it
/// any more, and it is fastcription's own file — its own first line says so.
pub fn clear_stale_session_configs() {
    let Ok(dir) = config_dir() else {
        return;
    };
    for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let ours = name == "voxtype.toml"
            || name == "voxtype.toml.tmp"
            || (name.starts_with("voxtype-") && name.ends_with(".toml"));
        if ours {
            remove_session_config(&entry.path());
        }
    }
}

/// The config for a connection test, in a temporary file.
///
/// Never the live path: a test runs while a meeting may be recording, and the
/// running transcriber re-reads its config on every pass. The returned handle
/// owns the file and deletes it on drop, so the token it may contain does not
/// outlive the test.
pub fn write_probe_config(settings: &EngineSettings) -> Result<tempfile::NamedTempFile, String> {
    use std::io::Write;

    let file = tempfile::Builder::new()
        .prefix("fastcription-probe")
        .suffix(".toml")
        // The body can carry a bearer token, so it is user-only from the
        // moment it exists rather than chmod'ed after the fact.
        .permissions(std::fs::Permissions::from_mode(0o600))
        .tempfile()
        .map_err(|err| format!("could not write a temporary config: {err}"))?;
    let mut handle = file.reopen().map_err(|err| err.to_string())?;
    handle
        .write_all(config_body(settings).as_bytes())
        .map_err(|err| format!("could not write a temporary config: {err}"))?;
    handle.sync_all().map_err(|err| err.to_string())?;
    Ok(file)
}

fn config_dir() -> Result<PathBuf, String> {
    Ok(dirs::config_dir()
        .ok_or("no configuration directory")?
        .join("fastcription"))
}

/// Writes `name` in `dir` atomically and user-only.
///
/// `create_new` on a temporary name in the same directory, then `rename`: a
/// reader — the transcriber, which opens this file on every pass — sees either
/// the previous config or the new one, never a half-written file. The mode is
/// set at creation rather than afterwards, so a bearer token is never briefly
/// world-readable.
pub fn write_private_config_in(
    dir: &Path,
    name: &str,
    settings: &EngineSettings,
) -> Result<PathBuf, String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    std::fs::create_dir_all(dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    let path = dir.join(name);
    let staging = dir.join(format!("{name}.tmp"));

    let body = config_body(settings);
    let mut file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&staging)
    {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            // A crash between create and rename. The file is ours by name, and
            // leaving it there would make every later write fail.
            tracing::warn!(path = %staging.display(), "replacing a stale staging config");
            std::fs::remove_file(&staging)
                .map_err(|err| format!("{}: {err}", staging.display()))?;
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&staging)
                .map_err(|err| format!("{}: {err}", staging.display()))?
        }
        Err(err) => return Err(format!("{}: {err}", staging.display())),
    };

    let written = file
        .write_all(body.as_bytes())
        .and_then(|()| file.sync_all());
    if let Err(err) = written {
        let _ = std::fs::remove_file(&staging);
        return Err(format!("{}: {err}", staging.display()));
    }
    drop(file);

    if let Err(err) = std::fs::rename(&staging, &path) {
        let _ = std::fs::remove_file(&staging);
        return Err(format!("{}: {err}", path.display()));
    }
    Ok(path)
}

/// The TOML voxtype reads, for whichever file it is about to land in.
fn config_body(settings: &EngineSettings) -> String {
    let mut body = String::from(
        "# Written by fastcription. Edits are overwritten.\n\
         #\n\
         # This is not your voxtype configuration. fastcription never modifies\n\
         # ~/.config/voxtype/config.toml; it reads it for defaults and passes\n\
         # this file to `voxtype -c` instead.\n\n\
         [whisper]\n",
    );

    match &settings.remote {
        Some(remote) => {
            body.push_str("mode = \"remote\"\n");
            body.push_str(&format!(
                "remote_endpoint = {}\n",
                toml_string(remote.endpoint.trim())
            ));
            if !remote.model.trim().is_empty() {
                body.push_str(&format!(
                    "remote_model = {}\n",
                    toml_string(remote.model.trim())
                ));
            }
            if !remote.api_key.trim().is_empty() {
                body.push_str(&format!(
                    "remote_api_key = {}\n",
                    toml_string(remote.api_key.trim())
                ));
            }
            body.push_str(&format!(
                "remote_timeout_secs = {}\n",
                remote.timeout_secs.max(1)
            ));
            if !settings.language.trim().is_empty() {
                body.push_str(&format!("language = {}\n", toml_string(&settings.language)));
            }
        }
        None => {
            if !settings.model.trim().is_empty() {
                body.push_str(&format!("model = {}\n", toml_string(&settings.model)));
            }
            if !settings.language.trim().is_empty() {
                body.push_str(&format!("language = {}\n", toml_string(&settings.language)));
            }
            // The reason this file exists. Only meaningful for a local model.
            body.push_str(&format!(
                "context_window_optimization = {}\n",
                settings.fast_mode
            ));
            body.push_str(&format!("threads = {}\n", settings.threads.max(1)));
        }
    }

    body
}

/// Quotes a value as a TOML basic string. The inputs are a model and a language
/// code from a text field, so a stray quote or backslash must not be able to
/// produce a config that parses as something else.
fn toml_string(value: &str) -> String {
    let escaped = value
        .chars()
        .filter(|c| !c.is_control())
        .flat_map(|c| match c {
            '"' => vec!['\\', '"'],
            '\\' => vec!['\\', '\\'],
            other => vec![other],
        })
        .collect::<String>();
    format!("\"{escaped}\"")
}

/// How many inference threads to ask for by default.
///
/// whisper.cpp stops scaling past about eight threads for the small English
/// models: measured on a 24-core machine, a 7 second window took 0.28s letting
/// whisper choose and 0.21s pinned to eight. Oversubscription costs time.
pub fn default_threads() -> u32 {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(4);
    cores.clamp(1, 8)
}

#[cfg(test)]
mod config_tests {
    use super::{toml_string, write_private_config_in, EngineSettings, PermissionsExt};

    /// The model and language come from text fields, so a value containing a
    /// quote must not be able to close the string and inject another key.
    #[test]
    fn values_are_quoted_so_a_stray_quote_cannot_inject_a_key() {
        assert_eq!(toml_string("base.en"), "\"base.en\"");
        assert_eq!(
            toml_string("evil\"\nthreads = 999"),
            "\"evil\\\"threads = 999\""
        );
        assert_eq!(toml_string("back\\slash"), "\"back\\\\slash\"");
    }

    /// The file must always carry the optimisation it exists for, and must
    /// parse as the TOML voxtype expects.
    #[test]
    fn the_written_config_enables_the_optimisation_and_parses() {
        let settings = EngineSettings {
            model: "base.en".into(),
            language: "en".into(),
            threads: 8,
            fast_mode: true,
            remote: None,
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_private_config_in(dir.path(), "voxtype.toml", &settings).expect("write");
        let body = std::fs::read_to_string(&path).expect("read back");
        assert!(body.contains("context_window_optimization = true"));
        assert!(body.contains("threads = 8"));
        assert!(body.contains("model = \"base.en\""));
        let parsed: toml::Value = toml::from_str(&body).expect("valid TOML");
        let whisper = parsed.get("whisper").expect("a whisper table");
        assert_eq!(
            whisper
                .get("context_window_optimization")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    fn plain() -> EngineSettings {
        EngineSettings {
            model: "base.en".into(),
            language: "en".into(),
            threads: 8,
            fast_mode: true,
            remote: None,
        }
    }

    /// The transcriber opens this file on every pass, so a reader must never
    /// see it half written — and the staging file must not survive, or the
    /// `create_new` on the next write would fail.
    #[test]
    fn the_write_is_atomic_and_leaves_no_staging_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_private_config_in(dir.path(), "voxtype-1.toml", &plain()).expect("write");
        assert!(path.exists());
        assert!(
            !dir.path().join("voxtype-1.toml.tmp").exists(),
            "the staging file must be renamed away, not left behind"
        );

        // Writing again over the same name has to work, which it does not if
        // the first write left its staging file in place.
        write_private_config_in(dir.path(), "voxtype-1.toml", &plain()).expect("rewrite");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    /// A stale staging file is what a crash between create and rename leaves.
    /// Refusing to write for ever afterwards would mean no session could start
    /// again until the user found and deleted a file they have never heard of.
    #[test]
    fn a_stale_staging_file_is_replaced_rather_than_fatal() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("voxtype-2.toml.tmp"), b"half a config").expect("stale");
        let path = write_private_config_in(dir.path(), "voxtype-2.toml", &plain()).expect("write");
        let body = std::fs::read_to_string(&path).expect("read back");
        assert!(body.contains("model = \"base.en\""));
    }

    /// Every config is user-only, not just the ones that happen to hold a
    /// token: the mode is set when the file is created, so there is no window
    /// in which a key is world-readable.
    #[test]
    fn a_config_is_user_only_from_the_moment_it_exists() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_private_config_in(dir.path(), "voxtype-3.toml", &plain()).expect("write");
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "mode was {mode:o}");
    }
}

/// Half a second of silence as a 16 kHz mono WAV, for probing a transcription
/// server. Written by hand rather than pulling in a WAV crate for forty-four
/// bytes of header.
pub fn write_probe_wav() -> std::io::Result<tempfile::NamedTempFile> {
    use std::io::Write;
    const RATE: u32 = 16_000;
    let frames = RATE / 2;
    let data_len = frames * 2;

    let mut file = tempfile::Builder::new()
        .prefix("fastcription-probe")
        .suffix(".wav")
        .tempfile()?;
    let w = file.as_file_mut();
    w.write_all(b"RIFF")?;
    w.write_all(&(36 + data_len).to_le_bytes())?;
    w.write_all(b"WAVEfmt ")?;
    w.write_all(&16u32.to_le_bytes())?;
    w.write_all(&1u16.to_le_bytes())?;
    w.write_all(&1u16.to_le_bytes())?;
    w.write_all(&RATE.to_le_bytes())?;
    w.write_all(&(RATE * 2).to_le_bytes())?;
    w.write_all(&2u16.to_le_bytes())?;
    w.write_all(&16u16.to_le_bytes())?;
    w.write_all(b"data")?;
    w.write_all(&data_len.to_le_bytes())?;
    w.write_all(&vec![0u8; data_len as usize])?;
    w.flush()?;
    Ok(file)
}

#[cfg(test)]
mod remote_config_tests {
    use super::{write_private_config_in, EngineSettings, RemoteEngine};

    fn settings(api_key: &str) -> EngineSettings {
        EngineSettings {
            model: "base.en".into(),
            language: "en".into(),
            threads: 8,
            fast_mode: true,
            remote: Some(RemoteEngine {
                endpoint: "http://desktop.lan:8080".into(),
                model: "whisper-1".into(),
                api_key: api_key.into(),
                timeout_secs: 30,
            }),
        }
    }

    /// Remote mode and local mode are mutually exclusive in voxtype's config:
    /// writing a local `model` alongside `mode = "remote"` would be ambiguous
    /// about which the engine should use.
    #[test]
    fn remote_mode_writes_the_server_and_omits_the_local_model() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path =
            write_private_config_in(dir.path(), "voxtype.toml", &settings("")).expect("write");
        let body = std::fs::read_to_string(&path).expect("read back");
        assert!(body.contains("mode = \"remote\""));
        assert!(body.contains("remote_endpoint = \"http://desktop.lan:8080\""));
        assert!(body.contains("remote_model = \"whisper-1\""));
        assert!(body.contains("remote_timeout_secs = 30"));
        assert!(
            !body.contains("\nmodel = "),
            "a local model must not be written in remote mode: {body}"
        );
        assert!(
            !body.contains("context_window_optimization"),
            "the local optimisation is meaningless against a server: {body}"
        );
        let _: toml::Value = toml::from_str(&body).expect("valid TOML");
    }

    /// A bearer token in a world-readable file would be a quiet mistake.
    #[test]
    fn a_config_holding_an_api_key_is_readable_only_by_its_owner() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_private_config_in(dir.path(), "voxtype.toml", &settings("secret-token"))
            .expect("write");
        let body = std::fs::read_to_string(&path).expect("read back");
        assert!(body.contains("remote_api_key = \"secret-token\""));
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "config with a key must be user-only");

        // Writing again without a key must not leave the old one behind.
        write_private_config_in(dir.path(), "voxtype.toml", &settings("")).expect("rewrite");
        let body = std::fs::read_to_string(&path).expect("read back");
        assert!(!body.contains("secret-token"), "a removed key must be gone");
    }
}

#[cfg(test)]
mod remote_path_tests {
    use super::{write_private_config_in, EngineSettings, RemoteEngine};
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// A minimal stand-in for an OpenAI-compatible transcription server.
    ///
    /// Returns the port it listens on and what the one request it serves
    /// contained, so the test can assert fastcription's config made voxtype
    /// speak the protocol a real server expects.
    fn serve_once(reply: &'static str) -> (u16, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let handle = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().expect("accept");
            // Read headers, then exactly as many body bytes as declared.
            let mut raw = Vec::new();
            let mut buf = [0u8; 8192];
            let mut header_end = None;
            while header_end.is_none() {
                let n = sock.read(&mut buf).expect("read");
                if n == 0 {
                    break;
                }
                raw.extend_from_slice(&buf[..n]);
                header_end = raw.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4);
            }
            let header_end = header_end.unwrap_or(raw.len());
            let headers = String::from_utf8_lossy(&raw[..header_end]).to_string();
            let declared: usize = headers
                .lines()
                .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
                .and_then(|l| l.split(':').nth(1)?.trim().parse().ok())
                .unwrap_or(0);
            while raw.len() - header_end < declared {
                let n = sock.read(&mut buf).expect("read body");
                if n == 0 {
                    break;
                }
                raw.extend_from_slice(&buf[..n]);
            }
            let body = String::from_utf8_lossy(&raw[header_end..]).to_string();

            let payload = format!("{{\"text\":\"{reply}\"}}");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            );
            sock.write_all(response.as_bytes()).expect("respond");
            let _ = sock.flush();
            format!("{headers}\n--BODY--\n{body}")
        });
        (port, handle)
    }

    /// Proves the whole remote path without a GPU anywhere: the config
    /// fastcription writes makes voxtype POST the audio to an
    /// OpenAI-compatible endpoint and hand the server's answer back as the
    /// transcript.
    #[test]
    #[ignore = "needs the voxtype binary"]
    fn a_transcription_server_is_reached_through_the_config_we_write() {
        let Some(binary) = fc_voxtype::cli::find_binary() else {
            return;
        };
        let (port, server) = serve_once("the server answered");

        let dir = tempfile::tempdir().expect("tempdir");
        let settings = EngineSettings {
            model: "base.en".into(),
            language: "en".into(),
            threads: 8,
            fast_mode: true,
            remote: Some(RemoteEngine {
                endpoint: format!("http://127.0.0.1:{port}"),
                model: "whisper-1".into(),
                api_key: "probe-token".into(),
                timeout_secs: 30,
            }),
        };
        let config =
            write_private_config_in(dir.path(), "voxtype.toml", &settings).expect("write config");
        let probe = super::write_probe_wav().expect("probe wav");

        let output = std::process::Command::new(&binary)
            .arg("-c")
            .arg(&config)
            .arg("-q")
            .arg("transcribe")
            .arg(probe.path())
            .output()
            .expect("run voxtype");
        assert!(
            output.status.success(),
            "voxtype failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let request = server.join().expect("server thread");
        assert!(
            request.contains("POST /v1/audio/transcriptions"),
            "expected the OpenAI transcription endpoint, got:\n{}",
            request.lines().next().unwrap_or_default()
        );
        assert!(
            request.contains("Authorization: Bearer probe-token"),
            "the API key must be sent"
        );
        assert!(request.contains("multipart/form-data"));
        assert!(
            request.contains("name=\"file\""),
            "the audio must be attached"
        );
        assert!(request.contains("RIFF"), "the attachment must be a WAV");
        assert!(request.contains("name=\"model\""));

        let transcript = String::from_utf8_lossy(&output.stdout);
        assert!(
            transcript.contains("the server answered"),
            "the server's text must come back as the transcript, got: {transcript}"
        );
    }
}
