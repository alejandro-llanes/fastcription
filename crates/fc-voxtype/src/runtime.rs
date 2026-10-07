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

use crossbeam_channel::Receiver;
use notify::{Event, RecursiveMode, Watcher};

use crate::error::{Result, VoxtypeError};

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
        Ok(Self { dir: PathBuf::from(base).join("voxtype") })
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

/// Spawns a background thread that watches `paths` and sends
/// [`RuntimeUpdate`]s until the returned [`Receiver`] is dropped. The thread
/// owns the `notify` watcher for its whole lifetime; dropping the receiver
/// does not explicitly stop it, but the OS reclaims the watch (and the
/// sender channel disconnects, so the next send is a no-op) once the process
/// that spawned it exits -- consistent with the rest of the app's "std
/// threads, no async runtime" design (ARCHITECTURE.md D8).
pub fn watch(paths: RuntimePaths) -> Result<Receiver<RuntimeUpdate>> {
    let (raw_tx, raw_rx) = crossbeam_channel::unbounded::<notify::Result<Event>>();
    let mut watcher = notify::recommended_watcher(raw_tx)?;

    let mut watching_dir = paths.dir.is_dir();
    if watching_dir {
        watcher.watch(&paths.dir, RecursiveMode::NonRecursive)?;
    } else {
        let parent = paths.dir.parent().unwrap_or(&paths.dir).to_path_buf();
        watcher.watch(&parent, RecursiveMode::NonRecursive)?;
    }

    let (tx, rx) = crossbeam_channel::unbounded();
    std::thread::spawn(move || {
        let mut watcher = watcher; // moved in; kept alive for this thread's life
        let dir = paths.dir.clone();
        let state_path = paths.state();
        let meeting_path = paths.meeting_state();

        for event in raw_rx {
            let Ok(event) = event else { continue };

            if !watching_dir {
                if event.paths.iter().any(|p| p == &dir) && dir.is_dir() {
                    let parent: Option<PathBuf> = dir.parent().map(Path::to_path_buf);
                    if watcher.watch(&dir, RecursiveMode::NonRecursive).is_ok() {
                        watching_dir = true;
                        if let Some(parent) = parent {
                            let _ = watcher.unwatch(&parent);
                        }
                        // Read the initial state immediately so the caller
                        // does not have to wait for a second filesystem event.
                        if let Some(state) =
                            std::fs::read_to_string(&state_path).ok().and_then(|s| DaemonState::parse(&s))
                        {
                            let _ = tx.send(RuntimeUpdate::State(state));
                        }
                    }
                }
                continue;
            }

            for path in &event.paths {
                if *path == state_path {
                    if let Some(state) =
                        std::fs::read_to_string(&state_path).ok().and_then(|s| DaemonState::parse(&s))
                    {
                        let _ = tx.send(RuntimeUpdate::State(state));
                    }
                } else if *path == meeting_path {
                    match std::fs::read_to_string(&meeting_path) {
                        Ok(s) => {
                            if let Some(m) = MeetingState::parse(&s) {
                                let _ = tx.send(RuntimeUpdate::Meeting(m));
                            }
                        }
                        Err(_) => {
                            let _ = tx.send(RuntimeUpdate::MeetingEnded);
                        }
                    }
                }
            }
        }
    });

    Ok(rx)
}
