//! `runtime.rs` parsing of the daemon's one-word `state` file and two-line
//! `meeting_state` file. Both real values (`idle`, and the absence of
//! `meeting_state` entirely) were captured from this machine's running
//! daemon; the in-meeting case is hand-written since no meeting was started
//! to capture it (the task constraints forbid starting one).

use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use fc_voxtype::runtime::{watch, DaemonState, MeetingState, RuntimePaths, RuntimeUpdate};

/// The three tests that start a real watcher run one at a time. They assert on
/// watcher threads of this process, and cargo runs the tests in this file
/// concurrently in one process, so overlapping them would make each one's
/// bookkeeping depend on the others.
static ONE_WATCHER_AT_A_TIME: Mutex<()> = Mutex::new(());

fn watcher_guard() -> MutexGuard<'static, ()> {
    ONE_WATCHER_AT_A_TIME
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

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
    let _serial = watcher_guard();
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

/// `systemctl --user restart voxtype` with a `RuntimeDirectory=` removes the
/// directory and creates a new one. An inotify watch on the removed directory
/// is blind from then on: it reports nothing about its replacement, so the app
/// would show a stale daemon state for the rest of its run.
#[test]
fn watch_follows_the_directory_through_a_remove_and_recreate() {
    let _serial = watcher_guard();
    let base = tempfile::tempdir().expect("tempdir");
    let voxtype_dir = base.path().join("voxtype");
    std::fs::create_dir_all(&voxtype_dir).expect("create voxtype dir");
    std::fs::write(voxtype_dir.join("state"), "idle").expect("write state");

    let paths = RuntimePaths {
        dir: voxtype_dir.clone(),
    };
    let rx = watch(paths).expect("watch an existing directory");

    // The first change is seen through the original watch.
    std::fs::write(voxtype_dir.join("state"), "recording").expect("write state");
    assert!(
        wait_for(&rx, Duration::from_secs(5), |u| matches!(
            u,
            RuntimeUpdate::State(DaemonState::Recording)
        )),
        "the watch on the original directory must work"
    );

    // The daemon restarts: directory gone, then a new one in its place.
    std::fs::remove_dir_all(&voxtype_dir).expect("remove voxtype dir");
    std::thread::sleep(Duration::from_millis(400));
    std::fs::create_dir_all(&voxtype_dir).expect("recreate voxtype dir");
    std::fs::write(voxtype_dir.join("state"), "transcribing").expect("write state");

    assert!(
        wait_for(&rx, Duration::from_secs(5), |u| matches!(
            u,
            RuntimeUpdate::State(DaemonState::Transcribing)
        )),
        "the watcher must follow the directory to its replacement"
    );
}

/// The app rebuilds this watcher whenever it rediscovers the runtime directory,
/// so a thread per attempt that never exits is a leak that grows with uptime.
#[test]
fn dropping_the_receiver_stops_the_watcher_thread() {
    let _serial = watcher_guard();
    let base = tempfile::tempdir().expect("tempdir");
    let voxtype_dir = base.path().join("voxtype");
    std::fs::create_dir_all(&voxtype_dir).expect("create voxtype dir");

    let before = thread_count();
    assert_eq!(
        before, 0,
        "no other watcher may be running: see watcher_guard"
    );
    let rx = watch(RuntimePaths {
        dir: voxtype_dir.clone(),
    })
    .expect("watch");
    std::fs::write(voxtype_dir.join("state"), "idle").expect("write state");
    assert!(
        wait_for(&rx, Duration::from_secs(5), |u| matches!(
            u,
            RuntimeUpdate::State(DaemonState::Idle)
        )),
        "the watcher must be running before the drop means anything"
    );

    drop(rx);
    // The loop wakes on its own every quarter second, so it notices without
    // needing another filesystem event.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && thread_count() > before {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        thread_count() <= before,
        "the watcher thread outlived its receiver: {} threads, started from {before}",
        thread_count()
    );
}

fn wait_for<F>(
    rx: &crossbeam_channel::Receiver<RuntimeUpdate>,
    timeout: Duration,
    mut want: F,
) -> bool
where
    F: FnMut(&RuntimeUpdate) -> bool,
{
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(update) = rx.recv_timeout(Duration::from_millis(100)) {
            if want(&update) {
                return true;
            }
        }
    }
    false
}

/// How many watcher threads this process has, by name, from `/proc/self/task`.
/// Counting them is how "the thread exited" becomes an assertion: `watch` hands
/// back a receiver, not a join handle, so there is nothing else to observe. By
/// name rather than in total, because the test harness and `notify` have threads
/// of their own coming and going.
fn thread_count() -> usize {
    std::fs::read_dir("/proc/self/task")
        .expect("read /proc/self/task")
        .filter_map(std::result::Result::ok)
        .filter(|task| {
            std::fs::read_to_string(task.path().join("comm"))
                .map(|comm| comm.trim() == "fc-vox-runtime")
                .unwrap_or(false)
        })
        .count()
}
