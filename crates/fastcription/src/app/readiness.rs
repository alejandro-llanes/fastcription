//! What has to be true before Start can do anything, and the one place the
//! remedies are written down.
//!
//! These strings used to exist only inside `start_or_resume`, which meant the
//! app told the user what was missing *after* they had chosen a source, pressed
//! Start and watched nothing happen. The same text now also fills the live
//! pane's first-run checklist, so a machine without voxtype says so before the
//! user has done anything — and says it once, in one wording.

use crate::app::App;
use crate::env::read_only_library;
use crate::i18n::{t, tf};

/// Raised when voxtype is not on `PATH`: at startup, and again if Start is
/// pressed anyway.
pub const NO_VOXTYPE: &str = "voxtype was not found on PATH, so nothing can be transcribed. \
     Install it and run `voxtype setup --download` to fetch a model.";

pub const NO_MODEL: &str =
    "No transcription model is installed. Run `voxtype setup model` to download one.";

pub const NO_SOURCES: &str = "No sound server answered, so there is nothing to record from. \
     Check that PipeWire or PulseAudio is running.";

pub const NO_LIBRARY: &str =
    "The conversation library at {} could not be opened, so recording is disabled.";

/// The commands the remedies name, as a user would type them.
const DOWNLOAD_COMMAND: &str = "voxtype setup --download";
const MODEL_COMMAND: &str = "voxtype setup model";
const SOUND_COMMAND: &str = "systemctl --user status pipewire wireplumber";

/// Whether a requirement is met, or still being looked into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Met,
    Unmet,
    /// The startup probe has not answered yet. Distinct from `Unmet` because
    /// the probes are subprocesses: for the first moments of a launch the
    /// source list is empty and the model list is empty, and reporting that as
    /// "no sound server" would be a lie that corrects itself.
    Waiting,
}

/// Something the user can put on the clipboard to get past a requirement.
pub struct Fix {
    /// The button's label: these are not all commands.
    pub button: &'static str,
    pub text: String,
}

pub struct Check {
    pub status: Status,
    /// What the requirement is, in two or three words.
    pub label: &'static str,
    /// Either what was found, or what to do about it not being found.
    pub detail: String,
    pub fix: Option<Fix>,
}

impl Check {
    fn met(label: &'static str, detail: String) -> Self {
        Self {
            status: Status::Met,
            label,
            detail,
            fix: None,
        }
    }

    fn unmet(label: &'static str, detail: &str, fix: Fix) -> Self {
        Self {
            status: Status::Unmet,
            label,
            detail: detail.to_owned(),
            fix: Some(fix),
        }
    }

    fn waiting(label: &'static str) -> Self {
        Self {
            status: Status::Waiting,
            label,
            detail: t("looking…").to_owned(),
            fix: None,
        }
    }
}

fn command(text: &'static str) -> Fix {
    Fix {
        button: t("Copy command"),
        text: text.to_owned(),
    }
}

/// The four things recording needs, in the order a user would fix them.
pub fn checks(app: &App) -> Vec<Check> {
    let probing = app.startup.is_some();
    vec![
        match &app.voxtype {
            Some(path) => Check::met(t("voxtype"), path.display().to_string()),
            None => Check::unmet(t("voxtype"), NO_VOXTYPE, command(DOWNLOAD_COMMAND)),
        },
        model_check(app, probing),
        sources_check(app, probing),
        library_check(app),
    ]
}

/// True when every requirement is met, which is what turns the checklist into
/// a single line.
pub fn all_met(checks: &[Check]) -> bool {
    checks.iter().all(|check| check.status == Status::Met)
}

/// Mirrors `start_or_resume`'s refusal exactly: a model typed into the settings
/// pane counts even when `voxtype info models` could not be read, because
/// transcription works perfectly well while that probe fails.
fn model_check(app: &App, probing: bool) -> Check {
    let configured = app.settings.model.trim();
    if !app.models.is_empty() {
        return Check::met(
            t("model"),
            tf(
                "{} \u{b7} {} installed",
                &[configured, &app.models.len().to_string()],
            ),
        );
    }
    if !configured.is_empty() {
        return Check::met(
            t("model"),
            tf("{} (voxtype did not list its models)", &[configured]),
        );
    }
    if probing {
        return Check::waiting(t("model"));
    }
    Check::unmet(t("model"), NO_MODEL, command(MODEL_COMMAND))
}

fn sources_check(app: &App, probing: bool) -> Check {
    if !app.sources.is_empty() {
        return Check::met(
            t("audio sources"),
            tf("{} to choose from", &[&app.sources.len().to_string()]),
        );
    }
    if probing {
        return Check::waiting(t("audio sources"));
    }
    Check::unmet(t("audio sources"), NO_SOURCES, command(SOUND_COMMAND))
}

fn library_check(app: &App) -> Check {
    let path = app.library_path.display().to_string();
    if app.store.is_none() {
        return Check {
            status: Status::Unmet,
            label: t("library"),
            detail: tf(NO_LIBRARY, &[&path]),
            fix: Some(Fix {
                button: t("Copy path"),
                text: path,
            }),
        };
    }
    if app.library_read_only {
        return Check {
            status: Status::Unmet,
            label: t("library"),
            detail: read_only_library(&app.library_path),
            fix: Some(Fix {
                button: t("Copy path"),
                text: path,
            }),
        };
    }
    Check::met(t("library"), path)
}
