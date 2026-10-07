//! `runtime.rs` parsing of the daemon's one-word `state` file and two-line
//! `meeting_state` file. Both real values (`idle`, and the absence of
//! `meeting_state` entirely) were captured from this machine's running
//! daemon; the in-meeting case is hand-written since no meeting was started
//! to capture it (the task constraints forbid starting one).

use fc_voxtype::runtime::{DaemonState, MeetingState, RuntimePaths};

#[test]
fn daemon_state_real_capture_idle() {
    assert_eq!(DaemonState::parse("idle"), Some(DaemonState::Idle));
}

#[test]
fn daemon_state_recording_and_transcribing() {
    assert_eq!(DaemonState::parse("recording"), Some(DaemonState::Recording));
    assert_eq!(DaemonState::parse("transcribing"), Some(DaemonState::Transcribing));
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
    let state = MeetingState::parse("active\n20261007-170000\n").expect("parses");
    assert_eq!(state.status, "active");
    assert_eq!(state.meeting_id, "20261007-170000");
}

#[test]
fn meeting_state_empty_text_is_none() {
    assert_eq!(MeetingState::parse(""), None);
}

#[test]
fn runtime_paths_derive_from_dir() {
    let paths = RuntimePaths { dir: "/run/user/1000/voxtype".into() };
    assert_eq!(paths.state(), std::path::PathBuf::from("/run/user/1000/voxtype/state"));
    assert_eq!(
        paths.meeting_state(),
        std::path::PathBuf::from("/run/user/1000/voxtype/meeting_state")
    );
    assert_eq!(paths.pid(), std::path::PathBuf::from("/run/user/1000/voxtype/pid"));
    assert_eq!(paths.version(), std::path::PathBuf::from("/run/user/1000/voxtype/version"));
    assert_eq!(
        paths.audio_sock(),
        std::path::PathBuf::from("/run/user/1000/voxtype/audio.sock")
    );
}
