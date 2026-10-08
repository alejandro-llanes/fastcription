//! Locating the `voxtype` binary and reading its diagnostic subcommands.
//!
//! Every probe here fails independently: a missing model must not hide a found
//! binary, and a binary at the wrong version must not hide installed models.
//! None of them panics or short-circuits on the first failure.
//!
//! The probes are split by cost, not by subject: [`probe_catalog`] is the ~10 ms
//! half (version, engines, models) and [`probe_health`] the ~290 ms half
//! (devices, accel), measured below. [`probe`] runs both, for a caller that
//! genuinely wants all five.
//!
//! voxtype's `info *` subcommands print for a human terminal, not a machine,
//! so every parser here works on whitespace-split tokens and indentation
//! rather than exact column positions. The first line of each subcommand's
//! output is a title banner (`Transcription engines`, `Model catalog  (...)`,
//! `Audio input devices`) and is always skipped; instructional footer lines
//! (`Switch with: ...`) are distinguished from data rows by indentation, not
//! by their wording, since the wording is the part voxtype is free to change.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::Deserialize;

use crate::bounded;
use crate::error::{Result, VoxtypeError};

/// Measured on this machine (voxtype 1.0.1): `--version`, `info engines` and
/// `info models` answer in 2-4 ms, `info accel` in ~35 ms, and `info devices`
/// in 239 ms cold (14 ms warm). Thirty seconds is therefore nowhere near any
/// working call — it is the line past which the binary is wedged rather than
/// slow. It has to exist because these run on the interface thread.
const INFO_TIMEOUT: Duration = Duration::from_secs(30);

/// Finds `voxtype` on `$PATH`, falling back to the path the Arch package
/// installs it at. Returns `None` rather than erroring so a caller can show
/// "voxtype is not installed" instead of a generic I/O failure.
pub fn find_binary() -> Option<PathBuf> {
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join("voxtype");
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    let fallback = PathBuf::from("/usr/bin/voxtype");
    is_executable(&fallback).then_some(fallback)
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Runs `voxtype <args>`, returning stdout on success. A non-zero exit is an
/// error carrying stderr, never an empty string standing in for failure, and a
/// binary that never answers is [`VoxtypeError::Timeout`] rather than a wait.
pub(crate) fn run(binary: &Path, args: &[&str]) -> Result<String> {
    run_within(binary, args, INFO_TIMEOUT)
}

/// The timeout is a parameter so the give-up path can be tested against a
/// deliberately slow fake binary without a test spending the real budget.
pub(crate) fn run_within(binary: &Path, args: &[&str], timeout: Duration) -> Result<String> {
    let mut command = Command::new(binary);
    command.args(args);
    let described = || format!("{} {}", binary.display(), args.join(" "));

    let Some(output) = bounded::output_within(&mut command, timeout)? else {
        return Err(VoxtypeError::Timeout {
            command: described(),
            after: timeout,
        });
    };
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(VoxtypeError::CommandFailed {
            command: described(),
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// `voxtype --version` prints `voxtype X.Y.Z`; this returns just `X.Y.Z`.
pub fn version(binary: &Path) -> Result<String> {
    let out = run(binary, &["--version"])?;
    out.split_whitespace()
        .last()
        .map(str::to_string)
        .ok_or_else(|| VoxtypeError::Parse {
            command: "--version",
            reason: "expected `voxtype X.Y.Z`".into(),
            text: out.clone(),
        })
}

/// One row of `voxtype info engines`: whether it is compiled into this
/// binary, and whether it is the one currently selected in config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineEntry {
    pub name: String,
    pub compiled: bool,
    pub active: bool,
}

/// Verified against voxtype 1.0.1 real output:
/// ```text
/// Transcription engines
///   compiled  whisper ● active
///             parakeet
/// ```
pub fn parse_engines(text: &str) -> Result<Vec<EngineEntry>> {
    let mut entries = Vec::new();
    for line in text.lines().skip(1) {
        if !line.starts_with(char::is_whitespace) {
            continue; // blank line, or an instructional footer like "Switch with: ..."
        }
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let Some(&first) = tokens.first() else {
            continue;
        };
        let (compiled, name) = if first == "compiled" {
            (true, tokens.get(1))
        } else {
            (false, tokens.first())
        };
        let Some(&name) = name else { continue };
        let active = tokens.contains(&"active");
        entries.push(EngineEntry {
            name: name.to_string(),
            compiled,
            active,
        });
    }
    if entries.is_empty() {
        return Err(VoxtypeError::Parse {
            command: "info engines",
            reason: "no engine rows found".into(),
            text: text.to_string(),
        });
    }
    Ok(entries)
}

/// One row of `voxtype info models`, scoped to the engine section it
/// appeared under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelEntry {
    pub engine: String,
    pub name: String,
    pub installed: bool,
    pub is_default: bool,
}

/// Verified against voxtype 1.0.1 real output:
/// ```text
/// whisper
///              tiny
///   installed  base.en (default)
///
/// parakeet
///              parakeet-tdt-0.6b-v3 (default)
/// ```
/// An engine header is any non-indented, non-empty line; a model row is any
/// indented line under the most recent header.
pub fn parse_models(text: &str) -> Result<Vec<ModelEntry>> {
    let mut entries = Vec::new();
    let mut current_engine: Option<String> = None;
    for line in text.lines().skip(1) {
        if line.trim().is_empty() {
            continue;
        }
        if !line.starts_with(char::is_whitespace) {
            current_engine = line.split_whitespace().next().map(str::to_string);
            continue;
        }
        let Some(engine) = current_engine.clone() else {
            continue;
        };
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let Some(&first) = tokens.first() else {
            continue;
        };
        let (installed, name) = if first == "installed" {
            (true, tokens.get(1))
        } else {
            (false, tokens.first())
        };
        let Some(&name) = name else { continue };
        entries.push(ModelEntry {
            engine,
            name: name.to_string(),
            installed,
            is_default: line.contains("(default)"),
        });
    }
    if entries.is_empty() {
        return Err(VoxtypeError::Parse {
            command: "info models",
            reason: "no model rows found".into(),
            text: text.to_string(),
        });
    }
    Ok(entries)
}

/// One row of `voxtype info devices`. Zero devices is a real, valid result
/// on a machine with no capture hardware wired up, so this never errors on
/// an empty list the way [`parse_engines`]/[`parse_models`] do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceEntry {
    pub name: String,
    pub is_default: bool,
}

pub fn parse_devices(text: &str) -> Vec<DeviceEntry> {
    text.lines()
        .skip(1)
        .filter(|l| l.starts_with(char::is_whitespace))
        .filter_map(|line| {
            let trimmed = line.trim();
            let name = trimmed.split_whitespace().next()?;
            Some(DeviceEntry {
                name: name.to_string(),
                is_default: trimmed.ends_with("(default)"),
            })
        })
        .collect()
}

/// `voxtype info accel`: whether the running daemon has GPU acceleration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccelInfo {
    pub state: Option<String>,
    pub backend: Option<String>,
    pub variant: Option<String>,
    pub daemon: Option<String>,
}

/// An all-`None` [`AccelInfo`] is indistinguishable from a successful parse of
/// output that said nothing this module recognises, so text with no recognised
/// key at all is an error (the crate's rule, see [`crate::error`]). Individual
/// missing keys stay `None`: `backend` is genuinely absent on a CPU-only
/// machine, and a daemon that is not running reports no `daemon` line.
pub fn parse_accel(text: &str) -> Result<AccelInfo> {
    let mut info = AccelInfo::default();
    let mut recognised = false;
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        match key.trim().to_lowercase().as_str() {
            "state" => info.state = Some(value.to_string()),
            "backend" => info.backend = Some(value.to_string()),
            "variant" => info.variant = Some(value.to_string()),
            "daemon" => info.daemon = Some(value.to_string()),
            _ => continue,
        }
        recognised = true;
    }
    if !recognised {
        return Err(VoxtypeError::Parse {
            command: "info accel",
            reason: "no state/backend/variant/daemon line found".into(),
            text: text.to_string(),
        });
    }
    Ok(info)
}

/// `voxtype status --format json --extended`, one line per state change.
///
/// `model`/`device`/`backend` are optional: they are absent on some builds
/// and before the daemon has loaded a model. `text`/`alt`/`class`/`tooltip`
/// are present on every real capture taken so far, but are kept required
/// only loosely -- see `#[serde(default)]` on `tooltip`, which this machine's
/// build always fills but older daemons might not.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct StatusInfo {
    pub text: String,
    pub alt: String,
    pub class: String,
    #[serde(default)]
    pub tooltip: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default)]
    pub backend: Option<String>,
}

impl StatusInfo {
    /// Builds an [`fc_core::EngineInfo`] for callers that need the shared
    /// domain type. The status stream has no `engine` field (it reports the
    /// model and backend, not which engine family loaded them), so the
    /// engine name has to come from the caller -- typically `config::Defaults`.
    pub fn engine_info(
        &self,
        engine: impl Into<String>,
        language: impl Into<String>,
    ) -> fc_core::EngineInfo {
        fc_core::EngineInfo {
            engine: engine.into(),
            model: self.model.clone().unwrap_or_default(),
            language: language.into(),
            backend: self.backend.clone(),
        }
    }
}

pub fn status(binary: &Path) -> Result<StatusInfo> {
    let out = run(binary, &["status", "--format", "json", "--extended"])?;
    serde_json::from_str(&out).map_err(|e| VoxtypeError::Parse {
        command: "status --format json --extended",
        reason: e.to_string(),
        text: out,
    })
}

/// What voxtype can be asked to do: its version and the engines and models it
/// knows about. Every field fails on its own, stringified so the struct stays
/// `Clone`.
///
/// Cheap on purpose. Measured on this machine (voxtype 1.0.1): `--version`,
/// `info engines` and `info models` cost 2-4 ms each, about 10 ms for the three.
/// This is what the settings pane needs, and keeping it apart from
/// [`probe_health`] is the difference between opening that pane costing 10 ms
/// and costing 290 ms.
#[derive(Debug, Clone)]
pub struct Catalog {
    pub version: std::result::Result<String, String>,
    pub engines: std::result::Result<Vec<EngineEntry>, String>,
    pub models: std::result::Result<Vec<ModelEntry>, String>,
}

/// What the machine can actually do right now: capture devices and GPU
/// acceleration.
///
/// The slow half. `info devices` measured 239 ms cold on this machine (14 ms
/// warm, so the cost is enumerating ALSA, not voxtype) and `info accel` ~35 ms.
/// Nothing in the app needs these to start recording, so nothing should pay for
/// them until it asks.
#[derive(Debug, Clone)]
pub struct Health {
    pub devices: std::result::Result<Vec<DeviceEntry>, String>,
    pub accel: std::result::Result<AccelInfo, String>,
}

pub fn probe_catalog() -> Catalog {
    let Some(bin) = find_binary() else {
        let missing = || VoxtypeError::BinaryNotFound.to_string();
        return Catalog {
            version: Err(missing()),
            engines: Err(missing()),
            models: Err(missing()),
        };
    };
    Catalog {
        version: version(&bin).map_err(|e| e.to_string()),
        engines: run(&bin, &["info", "engines"])
            .and_then(|t| parse_engines(&t))
            .map_err(|e| e.to_string()),
        models: run(&bin, &["info", "models"])
            .and_then(|t| parse_models(&t))
            .map_err(|e| e.to_string()),
    }
}

pub fn probe_health() -> Health {
    let Some(bin) = find_binary() else {
        let missing = || VoxtypeError::BinaryNotFound.to_string();
        return Health {
            devices: Err(missing()),
            accel: Err(missing()),
        };
    };
    Health {
        devices: run(&bin, &["info", "devices"])
            .map(|t| parse_devices(&t))
            .map_err(|e| e.to_string()),
        accel: run(&bin, &["info", "accel"])
            .and_then(|t| parse_accel(&t))
            .map_err(|e| e.to_string()),
    }
}

/// Both halves at once, plus where the binary is. Five subprocesses, so a
/// caller that reads two fields should call [`probe_catalog`] or
/// [`probe_health`] instead.
#[derive(Debug, Clone)]
pub struct Probe {
    pub binary: Option<PathBuf>,
    pub version: std::result::Result<String, String>,
    pub engines: std::result::Result<Vec<EngineEntry>, String>,
    pub models: std::result::Result<Vec<ModelEntry>, String>,
    pub devices: std::result::Result<Vec<DeviceEntry>, String>,
    pub accel: std::result::Result<AccelInfo, String>,
}

pub fn probe() -> Probe {
    let catalog = probe_catalog();
    let health = probe_health();
    Probe {
        binary: find_binary(),
        version: catalog.version,
        engines: catalog.engines,
        models: catalog.models,
        devices: health.devices,
        accel: health.accel,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bounded::fake_binary;

    /// These probes run on the interface thread, so a `voxtype` that never
    /// answers would freeze the window rather than report anything.
    #[test]
    fn a_voxtype_that_never_answers_times_out_instead_of_blocking() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = fake_binary(dir.path(), "slow-voxtype", "#!/bin/sh\nexec sleep 30\n");

        // A shorter budget than production's `INFO_TIMEOUT`: what is under test
        // is that a wedged binary becomes an error, not the constant's value.
        let budget = Duration::from_millis(200);
        let start = std::time::Instant::now();
        let err = run_within(&script, &["info", "engines"], budget)
            .expect_err("a wedged binary must be an error, not a wait");
        match err {
            VoxtypeError::Timeout { after, command } => {
                assert_eq!(after, budget);
                assert!(command.contains("info engines"), "{command}");
            }
            other => panic!("expected a timeout, got {other:?}"),
        }
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn a_prompt_binary_still_returns_its_stdout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = fake_binary(
            dir.path(),
            "quick-voxtype",
            "#!/bin/sh\necho voxtype 1.0.1\n",
        );
        let out = version(&script).expect("parses");
        assert_eq!(out, "1.0.1");
    }
}
