//! `runtime.rs` parsing of the daemon's one-word `state` file and two-line
//! `meeting_state` file. Both real values (`idle`, and the absence of
//! `meeting_state` entirely) were captured from this machine's running
//! daemon; the in-meeting case is hand-written since no meeting was started
//! to capture it (the task constraints forbid starting one).

use std::time::{Duration, Instant};

use fc_voxtype::runtime::{watch, DaemonState, MeetingState, RuntimePaths, RuntimeUpdate};

#[test]
fn daemon_state_real_capture_idle() {
    assert_eq!(DaemonState::parse("idle"), Some(DaemonState::Idle));
}

#[test]
fn daemon_state_recording_and_transcribing() {
    assert_eq!(
        DaemonState::parse("recording"),
        Some(DaemonState::Recording)
    );
    assert_eq!(
        DaemonState::parse("transcribing"),
        Some(DaemonState::Transcribing)
    );
}

#[test]
fn daemon_state_unknown_word_is_none_not_a_panic() {
    assert_eq!(DaemonState::parse("something-new"), None);
}

#[test]
fn daemon_state_tolerates_trailing_whitespace() {
    assert_eq!(DaemonState::parse("idle\n"), Some(DaemonState::Idle));
}

#[test]
fn meeting_state_two_lines_hand_written() {
    // "recording"/"paused" are the only two words the real daemon ever
    // writes here for an in-progress meeting (`write_meeting_state_file` in
    // voxtype's `src/daemon.rs`); `MeetingState::parse` itself is agnostic
    // to the specific word, it just splits two lines.
    let state = MeetingState::parse("recording\n20261007-170000\n").expect("parses");
    assert_eq!(state.status, "recording");
    assert_eq!(state.meeting_id, "20261007-170000");
}

#[test]
fn meeting_state_empty_text_is_none() {
    assert_eq!(MeetingState::parse(""), None);
}

#[test]
fn runtime_paths_derive_from_dir() {
    let paths = RuntimePaths {
        dir: "/run/user/1000/voxtype".into(),
    };
    assert_eq!(
        paths.state(),
        std::path::PathBuf::from("/run/user/1000/voxtype/state")
    );
    assert_eq!(
        paths.meeting_state(),
        std::path::PathBuf::from("/run/user/1000/voxtype/meeting_state")
    );
    assert_eq!(
        paths.pid(),
        std::path::PathBuf::from("/run/user/1000/voxtype/pid")
    );
    assert_eq!(
        paths.version(),
        std::path::PathBuf::from("/run/user/1000/voxtype/version")
    );
    assert_eq!(
        paths.audio_sock(),
        std::path::PathBuf::from("/run/user/1000/voxtype/audio.sock")
    );
}

/// `watch()` is handed a directory that does not exist yet (the common case:
/// the app starts before voxtype's daemon has ever run this boot) and must
/// notice once the daemon creates it -- including picking up a meeting that
/// was already in progress the moment the directory shows up, not just the
/// bare `state` word. This exercises the real `notify` watcher end to end,
/// not just the pure parsers above.
#[test]
fn watch_emits_initial_state_and_meeting_once_the_directory_appears() {
    let base = tempfile::tempdir().expect("tempdir");
    let voxtype_dir = base.path().join("voxtype");
    assert!(!voxtype_dir.exists(), "directory must not exist yet");

    let paths = RuntimePaths {
        dir: voxtype_dir.clone(),
    };
    let rx = watch(paths).expect("watch starts against the not-yet-existing directory");

    // The daemon starts: the directory appears with a meeting already
    // in progress, as if this watcher attached after the daemon did.
    std::fs::create_dir_all(&voxtype_dir).expect("create voxtype dir");
    std::fs::write(voxtype_dir.join("state"), "recording").expect("write state");
    std::fs::write(voxtype_dir.join("meeting_state"), "recording\nabc-123")
        .expect("write meeting_state");

    let mut saw_state = false;
    let mut saw_meeting = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !(saw_state && saw_meeting) {
        if let Ok(update) = rx.recv_timeout(Duration::from_millis(200)) {
            match update {
                RuntimeUpdate::State(DaemonState::Recording) => saw_state = true,
                RuntimeUpdate::Meeting(m) if m.meeting_id == "abc-123" => saw_meeting = true,
                _ => {}
            }
        }
    }
    assert!(
        saw_state,
        "expected an initial State(Recording) once the directory appeared"
    );
    assert!(
        saw_meeting,
        "expected an initial Meeting(..) once the directory appeared, not just State"
    );
}
