//! Watches `$XDG_RUNTIME_DIR/voxtype/` for the daemon's state files.
//!
//! Verified on this machine while the daemon was running:
//! `$XDG_RUNTIME_DIR/voxtype/` holds `state` (one word), `pid`, `version`,
//! `audio.sock`, and -- only while a meeting is active -- `meeting_state`
//! (two lines: status, then meeting id). `voxtype.lock` also lives there but
//! is not this module's concern.
//!
//! The directory does not exist until the daemon has run at least once this
//! boot, so [`watch`] never fails just because voxtype has not been started
//! yet: it watches the parent (`$XDG_RUNTIME_DIR` itself, which always
//! exists under systemd-logind) until the `voxtype` subdirectory appears,
//! then re-watches that directory directly.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use notify::event::{AccessKind, AccessMode};
use notify::{Event, EventKind, RecursiveMode, Watcher};

use crate::error::{Result, VoxtypeError};

/// How often the watcher thread wakes up with no filesystem event to look at,
/// so that dropping the receiver stops it within about that long rather than
/// whenever the daemon next happens to write a file.
const IDLE_TICK: Duration = Duration::from_millis(250);

/// State the daemon reports via the `state` file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonState {
    Idle,
    Recording,
    Transcribing,
}

impl DaemonState {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "idle" => Some(Self::Idle),
            "recording" => Some(Self::Recording),
            "transcribing" => Some(Self::Transcribing),
            _ => None,
        }
    }
}

/// The two-line `meeting_state` file: `status` then `meeting_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeetingState {
    pub status: String,
    pub meeting_id: String,
}

impl MeetingState {
    pub fn parse(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        let status = lines.next()?.trim().to_string();
        let meeting_id = lines.next().unwrap_or("").trim().to_string();
        if status.is_empty() {
            return None;
        }
        Some(Self { status, meeting_id })
    }
}

/// The files and sockets fastcription cares about under
/// `$XDG_RUNTIME_DIR/voxtype/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePaths {
    pub dir: PathBuf,
}

impl RuntimePaths {
    pub fn discover() -> Result<Self> {
        let base = std::env::var_os("XDG_RUNTIME_DIR").ok_or(VoxtypeError::NoRuntimeDir)?;
        Ok(Self {
            dir: PathBuf::from(base).join("voxtype"),
        })
    }

    pub fn state(&self) -> PathBuf {
        self.dir.join("state")
    }

    pub fn meeting_state(&self) -> PathBuf {
        self.dir.join("meeting_state")
    }

    pub fn pid(&self) -> PathBuf {
        self.dir.join("pid")
    }

    pub fn version(&self) -> PathBuf {
        self.dir.join("version")
    }

    pub fn audio_sock(&self) -> PathBuf {
        self.dir.join("audio.sock")
    }
}

/// One snapshot read right now. The UI needs this to show correct state
/// before the watcher's first event arrives, and also to have something to
/// show at all when the daemon has never run and the directory is absent.
pub fn read_now(paths: &RuntimePaths) -> (Option<DaemonState>, Option<MeetingState>) {
    let state = std::fs::read_to_string(paths.state())
        .ok()
        .and_then(|s| DaemonState::parse(&s));
    let meeting = std::fs::read_to_string(paths.meeting_state())
        .ok()
        .and_then(|s| MeetingState::parse(&s));
    (state, meeting)
}

/// An update pushed by the background watcher thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeUpdate {
    State(DaemonState),
    Meeting(MeetingState),
    /// `meeting_state` was removed: the meeting ended (or was never started).
    MeetingEnded,
}

/// The receiving end of [`watch`]: a [`Receiver`] of updates that is also the
/// watcher thread's reason to keep running. Dropping it stops the thread within
/// about [`IDLE_TICK`], whether or not the daemon ever writes again.
pub struct Updates {
    updates: Receiver<RuntimeUpdate>,
    /// Never sent on. The thread holds the other end and asks it on every idle
    /// tick whether this side is gone -- the only way a sender can learn that
    /// its receiver was dropped without having something to send. The updates
    /// channel alone could not tell it: a daemon that has stopped writes
    /// nothing more, so there would be nothing to send, for ever.
    _alive: Sender<()>,
}

impl std::ops::Deref for Updates {
    type Target = Receiver<RuntimeUpdate>;

    fn deref(&self) -> &Receiver<RuntimeUpdate> {
        &self.updates
    }
}

/// Spawns a background thread that watches `paths` and sends
/// [`RuntimeUpdate`]s until the returned [`Updates`] is dropped.
///
/// The thread owns the `notify` watcher for its whole lifetime and stops within
/// about [`IDLE_TICK`] of the receiver being dropped — it has to be able to
/// stop, because the app rebuilds this watcher whenever the runtime directory is
/// rediscovered, and a thread per attempt that never exits is a leak that grows
/// with uptime. Consistent with the rest of the app's "std threads, no async
/// runtime" design (ARCHITECTURE.md D8).
///
/// The watch itself follows the directory rather than assuming it stays put: the
/// daemon's runtime directory can be removed and recreated (`systemctl --user
/// restart voxtype` with `RuntimeDirectory=` does exactly that), and an inotify
/// watch on a removed directory is simply blind — it reports nothing about the
/// new one, for ever.
pub fn watch(paths: RuntimePaths) -> Result<Updates> {
    let (raw_tx, raw_rx) = crossbeam_channel::unbounded::<notify::Result<Event>>();
    let mut watcher = notify::recommended_watcher(raw_tx)?;

    let watching_dir = paths.dir.is_dir()
        && watcher
            .watch(&paths.dir, RecursiveMode::NonRecursive)
            .is_ok();
    if !watching_dir {
        let parent = paths.dir.parent().unwrap_or(&paths.dir).to_path_buf();
        watcher.watch(&parent, RecursiveMode::NonRecursive)?;
        // The directory can appear between that check and this watch attaching,
        // and that creation produces no event this watcher would ever see. The
        // loop re-checks `is_dir()` on every tick rather than only on an event
        // naming the directory, which closes the race without needing the
        // creation event at all.
    }

    let (tx, rx) = crossbeam_channel::unbounded();
    let (alive_tx, alive_rx) = crossbeam_channel::bounded::<()>(0);
    // Named so that "this thread exited when its receiver was dropped" is
    // something a test can actually check: `watch` hands back a receiver, not a
    // join handle. Under 15 characters, which is all `/proc/*/comm` keeps.
    std::thread::Builder::new()
        .name("fc-vox-runtime".into())
        .spawn(move || run_watch_loop(watcher, raw_rx, tx, alive_rx, paths, watching_dir))
        .map_err(VoxtypeError::Io)?;

    Ok(Updates {
        updates: rx,
        _alive: alive_tx,
    })
}

/// The watcher thread's body. Split out from [`watch`] so a test can run it
/// directly and watch it exit when the receiver goes away.
fn run_watch_loop(
    mut watcher: impl Watcher,
    raw_rx: Receiver<notify::Result<Event>>,
    tx: Sender<RuntimeUpdate>,
    alive: Receiver<()>,
    paths: RuntimePaths,
    mut watching_dir: bool,
) {
    let dir = paths.dir.clone();
    let state_path = paths.state();
    let meeting_path = paths.meeting_state();

    // Nothing is emitted for a directory that already existed: the caller has
    // `read_now` for the initial snapshot, and this reports changes.
    loop {
        let event = match raw_rx.recv_timeout(IDLE_TICK) {
            Ok(Ok(event)) => Some(event),
            // A malformed event still means something happened, and the ticks
            // below do not depend on which.
            Ok(Err(_)) => None,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => None,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
        };
        // Every tick, not only when there is something to send: see `Updates`.
        if matches!(
            alive.try_recv(),
            Err(crossbeam_channel::TryRecvError::Disconnected)
        ) {
            return;
        }

        if !watching_dir {
            // Waiting for the directory to appear. Checked on every tick, not
            // only on an event naming it: the creation can be missed entirely
            // (see the race above), and `is_dir` costs one stat.
            if dir.is_dir() && watcher.watch(&dir, RecursiveMode::NonRecursive).is_ok() {
                watching_dir = true;
                if let Some(parent) = dir.parent() {
                    let _ = watcher.unwatch(parent);
                }
                // Both files, mirroring `read_now`: a meeting can already be in
                // progress by the time this directory shows up.
                if emit_current(&tx, &state_path, &meeting_path).is_err() {
                    return;
                }
            }
            continue;
        }

        if !dir.is_dir() {
            // The directory was removed (or renamed away). An inotify watch on
            // a removed directory is blind, not merely quiet: it will never
            // report anything about the directory that takes its place. So drop
            // it, watch the parent again, and let the tick above notice the
            // replacement -- which it may have to do immediately, since the
            // daemon can recreate the directory before this even runs.
            let _ = watcher.unwatch(&dir);
            watching_dir = false;
            let parent: Option<PathBuf> = dir.parent().map(Path::to_path_buf);
            if let Some(parent) = parent {
                let _ = watcher.watch(&parent, RecursiveMode::NonRecursive);
            }
            // Whatever meeting was in progress is certainly not any more.
            if tx.send(RuntimeUpdate::MeetingEnded).is_err() {
                return;
            }
            continue;
        }

        let Some(event) = event else { continue };
        if !is_change(&event.kind) {
            continue;
        }
        for path in &event.paths {
            if *path == state_path {
                if let Some(state) = read_state(&state_path) {
                    if tx.send(RuntimeUpdate::State(state)).is_err() {
                        return;
                    }
                }
            } else if *path == meeting_path {
                let update = match read_meeting(&meeting_path) {
                    Some(meeting) => RuntimeUpdate::Meeting(meeting),
                    None => RuntimeUpdate::MeetingEnded,
                };
                if tx.send(update).is_err() {
                    return;
                }
            }
        }
    }
}

/// Whether an event can mean a file's contents are different now.
///
/// `notify`'s inotify backend subscribes to `IN_OPEN`, so every read of a file
/// in the watched directory arrives here as `Access(Open)` naming that file --
/// the service monitor's `read_now`, and the `read_state` this loop does in
/// answer to an event. Taking those for changes made the watcher its own event
/// source: each read was the next event, the loop turned over as fast as a
/// four-byte file can be opened, and every turn queued an update that the
/// consumer drained far more slowly -- about 13 MB a second, for as long as
/// the app was open, idle or not (v0.1.0). A read never changes a file, so no
/// `Access` event counts except the one that ends a write.
fn is_change(kind: &EventKind) -> bool {
    match kind {
        EventKind::Access(AccessKind::Close(AccessMode::Write)) => true,
        EventKind::Access(_) => false,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => true,
        EventKind::Any | EventKind::Other => true,
    }
}

fn read_state(path: &Path) -> Option<DaemonState> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| DaemonState::parse(&s))
}

fn read_meeting(path: &Path) -> Option<MeetingState> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| MeetingState::parse(&s))
}

/// Sends whatever the two state files say right now. `Err(())` means the
/// receiver is gone and the caller should stop.
fn emit_current(
    tx: &Sender<RuntimeUpdate>,
    state_path: &Path,
    meeting_path: &Path,
) -> std::result::Result<(), ()> {
    if let Some(state) = read_state(state_path) {
        tx.send(RuntimeUpdate::State(state)).map_err(|_| ())?;
    }
    if let Some(meeting) = read_meeting(meeting_path) {
        tx.send(RuntimeUpdate::Meeting(meeting)).map_err(|_| ())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use notify::event::{CreateKind, DataChange, MetadataKind, ModifyKind, RemoveKind, RenameMode};

    use super::*;

    /// The inotify backend's subscription, kind by kind: the one a read
    /// produces must not count, everything a write or a rename produces must.
    #[test]
    fn a_read_is_not_a_change_but_every_kind_of_write_is() {
        let open = EventKind::Access(AccessKind::Open(AccessMode::Any));
        assert!(!is_change(&open), "IN_OPEN is what a read looks like");
        assert!(!is_change(&EventKind::Access(AccessKind::Read)));
        assert!(!is_change(&EventKind::Access(AccessKind::Close(
            AccessMode::Read
        ))));
        assert!(!is_change(&EventKind::Access(AccessKind::Any)));

        // A write in place: truncate, write, close.
        assert!(is_change(&EventKind::Modify(ModifyKind::Data(
            DataChange::Any
        ))));
        assert!(is_change(&EventKind::Access(AccessKind::Close(
            AccessMode::Write
        ))));
        // An atomic replace: a new file renamed over the old one.
        assert!(is_change(&EventKind::Create(CreateKind::File)));
        assert!(is_change(&EventKind::Modify(ModifyKind::Name(
            RenameMode::To
        ))));
        assert!(is_change(&EventKind::Remove(RemoveKind::File)));
        assert!(is_change(&EventKind::Modify(ModifyKind::Metadata(
            MetadataKind::Any
        ))));
        // A queue overflow says "look again", which is a change for our purposes.
        assert!(is_change(&EventKind::Other));
    }
}
