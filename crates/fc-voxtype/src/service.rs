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

use crate::error::{Result, VoxtypeError};

pub const UNIT: &str = "voxtype.service";

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
    let output = Command::new("systemctl")
        .args([
            "--user",
            "show",
            "-p",
            "ActiveState,SubState,UnitFileState",
            unit,
        ])
        .output()?;
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
    let output = Command::new("systemctl")
        .args(["--user", verb, UNIT])
        .output()?;
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
