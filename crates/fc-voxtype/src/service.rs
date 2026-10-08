//! Control of the `voxtype.service` systemd **user** unit.
//!
//! Shells out to `systemctl --user` rather than linking a D-Bus client: the
//! live transcription path never needs the daemon at all (ARCHITECTURE.md
//! §7), so this module exists only for the user's own dictation workflow and
//! for meeting-mode delegation. That does not justify pulling in an async
//! D-Bus dependency.
//!
//! This crate's tests exercise [`parse_show`] against captured
//! `systemctl --user show` text, never by actually starting or stopping the
//! unit -- the service on the machine this was written on is the user's own
//! dictation daemon, running for real.

use std::process::Command;
use std::time::Duration;

use crate::bounded;
use crate::error::{Result, VoxtypeError};

pub const UNIT: &str = "voxtype.service";

/// `systemctl --user` talks to `systemd --user` over the session bus, and a bus
/// that is not answering (a `systemd --user` reload, a D-Bus hiccup) makes the
/// call wait indefinitely. [`status`] runs on the interface thread, so that wait
/// is a frozen window. Ten seconds is far past any healthy call — `show` is a
/// few milliseconds — while still leaving room for a `start` that is slow
/// because the unit itself is slow to come up.
const SYSTEMCTL_TIMEOUT: Duration = Duration::from_secs(10);

/// `systemctl --user show -p ActiveState,SubState,UnitFileState` for
/// [`UNIT`], already split into the three properties fastcription needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceStatus {
    /// systemd knows this unit. `active_state`/`sub_state` are systemd's own
    /// vocabulary (`active`/`inactive`/`failed`/.../`running`/`dead`/...);
    /// kept as raw strings rather than an enum so a value this module has
    /// not seen yet is not silently coerced into the wrong case.
    Found {
        active_state: String,
        sub_state: String,
        unit_file_state: String,
    },
    /// Verified on this machine: `systemctl --user show` on a unit name it
    /// has never heard of still exits 0 and prints
    /// `ActiveState=inactive`/`SubState=dead`/`UnitFileState=` (empty) rather
    /// than failing. An empty `UnitFileState` is the signal systemd gives for
    /// "no such unit", and plenty of users will not have installed voxtype's
    /// unit at all, so this gets its own case rather than being reported as
    /// "inactive".
    NotInstalled,
}

impl ServiceStatus {
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Found { active_state, .. } if active_state == "active")
    }
}

/// Parses `systemctl --user show -p ActiveState,SubState,UnitFileState`
/// output: `KEY=VALUE` pairs, one per line, in no guaranteed order.
pub fn parse_show(text: &str) -> ServiceStatus {
    let mut active_state = String::new();
    let mut sub_state = String::new();
    let mut unit_file_state = String::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "ActiveState" => active_state = value.to_string(),
            "SubState" => sub_state = value.to_string(),
            "UnitFileState" => unit_file_state = value.to_string(),
            _ => {}
        }
    }
    if unit_file_state.is_empty() {
        ServiceStatus::NotInstalled
    } else {
        ServiceStatus::Found {
            active_state,
            sub_state,
            unit_file_state,
        }
    }
}

fn show(unit: &str) -> Result<String> {
    show_with("systemctl", unit, SYSTEMCTL_TIMEOUT)
}

/// The program and timeout are parameters so the give-up path can be tested
/// against a deliberately slow fake binary — without putting one on `PATH`,
/// which would mean mutating the environment under every other test, and
/// without a test spending the real budget.
fn show_with(program: &str, unit: &str, timeout: Duration) -> Result<String> {
    let args = [
        "--user",
        "show",
        "-p",
        "ActiveState,SubState,UnitFileState",
        unit,
    ];
    let mut command = Command::new(program);
    command.args(args);

    let Some(output) = bounded::output_within(&mut command, timeout)? else {
        return Err(VoxtypeError::Timeout {
            command: format!("{program} {}", args.join(" ")),
            after: timeout,
        });
    };
    if !output.status.success() {
        return Err(VoxtypeError::CommandFailed {
            command: format!("systemctl --user show -p ActiveState,SubState,UnitFileState {unit}"),
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The richer status: raw systemd properties, with the not-installed case
/// told apart from a generic failure.
pub fn status() -> Result<ServiceStatus> {
    show(UNIT).map(|text| parse_show(&text))
}

/// Just whether the unit is `active`. `NotInstalled` reads as `false`.
pub fn is_active() -> Result<bool> {
    Ok(status()?.is_active())
}

fn action(verb: &str) -> Result<()> {
    action_with("systemctl", verb, SYSTEMCTL_TIMEOUT)
}

fn action_with(program: &str, verb: &str, timeout: Duration) -> Result<()> {
    let mut command = Command::new(program);
    command.args(["--user", verb, UNIT]);

    let Some(output) = bounded::output_within(&mut command, timeout)? else {
        return Err(VoxtypeError::Timeout {
            command: format!("{program} --user {verb} {UNIT}"),
            after: timeout,
        });
    };
    if output.status.success() {
        Ok(())
    } else {
        Err(VoxtypeError::CommandFailed {
            command: format!("systemctl --user {verb} {UNIT}"),
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

pub fn start() -> Result<()> {
    action("start")
}

pub fn stop() -> Result<()> {
    action("stop")
}

pub fn restart() -> Result<()> {
    action("restart")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_systemctl(dir: &std::path::Path, name: &str, body: &str) -> String {
        crate::bounded::fake_binary(dir, name, body)
            .to_str()
            .expect("utf8 path")
            .to_string()
    }

    /// `status` runs on the interface thread, so a `systemctl` waiting on a bus
    /// that will not answer must become an error rather than a frozen window.
    /// Uses a fake binary throughout: the real unit on this machine is the
    /// user's own dictation daemon, and the task forbids touching it.
    #[test]
    fn a_systemctl_that_never_answers_times_out_instead_of_blocking() {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = fake_systemctl(dir.path(), "wedged-systemctl", "#!/bin/sh\nexec sleep 30\n");

        let budget = Duration::from_millis(200);
        let start = std::time::Instant::now();
        let err = show_with(&program, UNIT, budget).expect_err("a wedged systemctl is an error");
        match err {
            VoxtypeError::Timeout { after, command } => {
                assert_eq!(after, budget);
                assert!(command.contains("ActiveState"), "{command}");
            }
            other => panic!("expected a timeout, got {other:?}"),
        }
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "took {:?}",
            start.elapsed()
        );
    }

    /// Starting and stopping the unit is a button in the settings pane, so the
    /// same wait would freeze the window there.
    #[test]
    fn a_wedged_start_times_out_too() {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = fake_systemctl(dir.path(), "wedged-systemctl", "#!/bin/sh\nexec sleep 30\n");

        let err = action_with(&program, "start", Duration::from_millis(200))
            .expect_err("a wedged systemctl is an error");
        assert!(matches!(err, VoxtypeError::Timeout { .. }), "got {err:?}");
    }

    #[test]
    fn a_prompt_systemctl_is_parsed_as_usual() {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = fake_systemctl(
            dir.path(),
            "prompt-systemctl",
            "#!/bin/sh\necho ActiveState=active\necho SubState=running\necho UnitFileState=enabled\n",
        );
        let text = show_with(&program, UNIT, Duration::from_secs(5)).expect("prompt");
        assert!(parse_show(&text).is_active());
    }
}
