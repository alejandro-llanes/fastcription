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

/// Raised when voxtype transcribes on this machine's CPU and no transcription
/// server is configured.
///
/// Not a refusal: recording works and the transcript is correct. It is the
/// *latency* that fails, and with it the only thing this application is for —
/// measured on one desktop, a 7-second window of speech took 85 ms to encode
/// on a GPU and 2.8 seconds on 24 CPU threads, against a pass budget of about
/// a second. A reader cannot follow a conversation from captions that arrive
/// after it has moved on, and the gap grows for as long as the meeting lasts.
pub const CPU_ONLY: &str = "voxtype is transcribing on this machine's CPU, which cannot keep \
     up with speech — the captions will fall behind and keep falling. Turn on GPU \
     acceleration, or send the audio to a machine that has a GPU \
     (Settings → Transcription server).";

/// The commands the remedies name, as a user would type them.
const DOWNLOAD_COMMAND: &str = "voxtype setup --download";
const MODEL_COMMAND: &str = "voxtype setup model";
const SOUND_COMMAND: &str = "systemctl --user status pipewire wireplumber";
/// voxtype ships prebuilt variants and switches between them itself, so this
/// asks it to pick rather than naming a backend fastcription would then have to
/// keep in step with voxtype's own list.
const GPU_COMMAND: &str = "voxtype setup gpu --enable";

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
    /// Something works, but not well enough for what it is for. Separate from
    /// `Unmet` because the checklist's other job is to explain why Start will
    /// refuse, and an advisory does not make it refuse — it must not hold the
    /// checklist open on a machine where there is nothing to fix.
    Advisory,
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

    fn advisory(label: &'static str, detail: &str, fix: Fix) -> Self {
        Self {
            status: Status::Advisory,
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

/// The four things recording needs, in the order a user would fix them, and
/// then whether it will be fast enough to read along with.
pub fn checks(app: &App) -> Vec<Check> {
    let probing = app.startup.is_some();
    let mut checks = vec![
        match &app.voxtype {
            Some(path) => Check::met(t("voxtype"), path.display().to_string()),
            None => Check::unmet(t("voxtype"), t(NO_VOXTYPE), command(DOWNLOAD_COMMAND)),
        },
        model_check(app, probing),
        sources_check(app, probing),
        library_check(app),
    ];
    checks.extend(speed_check(app));
    checks
}

/// True when nothing is stopping a recording, which is what turns the
/// checklist into a single line.
///
/// An advisory does not count against it — it is not a reason to withhold
/// "Ready", only something to say alongside it, which the live pane does.
pub fn all_met(checks: &[Check]) -> bool {
    checks
        .iter()
        .all(|check| matches!(check.status, Status::Met | Status::Advisory))
}

/// Whether the transcription can keep up with the conversation.
///
/// `None` in the two cases where this machine's backend says nothing about the
/// latency: a transcription server is configured, so the model runs elsewhere;
/// or voxtype reported no backend at all, which is what `voxtype status
/// --extended` does when its daemon is not running. Guessing in either case
/// would mean warning about a problem the user may not have.
fn speed_check(app: &App) -> Option<Check> {
    if app.settings.remote_enabled {
        return None;
    }
    let backend = app.engine.backend.as_deref()?;
    if accelerated(backend) {
        return Some(Check::met(t("speed"), backend.to_owned()));
    }
    Some(Check::advisory(
        t("speed"),
        t(CPU_ONLY),
        command(GPU_COMMAND),
    ))
}

/// voxtype names its backends `CPU (AVX2)`, `CPU (AVX-512)`, `GPU (Vulkan)`,
/// and more as it gains them. Recognising the CPU ones, rather than listing the
/// accelerators, is the direction that fails quietly: a backend this was never
/// built against reads as accelerated and says nothing, where the reverse would
/// warn every user of it about a problem they do not have.
///
/// It cannot tell *which* GPU. On a machine with an integrated and a discrete
/// one, voxtype takes the first it finds — `docs/SERVER.md` covers pinning it
/// to the fast one.
fn accelerated(backend: &str) -> bool {
    !backend.to_lowercase().contains("cpu")
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
    Check::unmet(t("model"), t(NO_MODEL), command(MODEL_COMMAND))
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
    Check::unmet(t("audio sources"), t(NO_SOURCES), command(SOUND_COMMAND))
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

#[cfg(test)]
mod tests {
    use super::{accelerated, all_met, Check, Fix, Status};

    fn fix() -> Fix {
        Fix {
            button: "Copy command",
            text: "command".to_owned(),
        }
    }

    /// The backend names voxtype actually prints, from `voxtype status
    /// --format json --extended` and `voxtype setup gpu --status`.
    #[test]
    fn cpu_backends_are_not_accelerated() {
        assert!(!accelerated("CPU (AVX2)"));
        assert!(!accelerated("CPU (AVX-512)"));
        assert!(!accelerated("cpu"));
    }

    #[test]
    fn gpu_backends_are_accelerated() {
        assert!(accelerated("GPU (Vulkan)"));
        assert!(accelerated("CUDA"));
        assert!(accelerated("MIGraphX"));
    }

    /// The fail-quiet direction, and the reason `accelerated` is written as a
    /// CPU test rather than a list of accelerators: a backend added to voxtype
    /// after this was written must not make the app warn everyone using it.
    #[test]
    fn an_unrecognised_backend_says_nothing() {
        assert!(accelerated("Metal"));
        assert!(accelerated("something invented in 2030"));
    }

    #[test]
    fn an_advisory_does_not_hold_the_checklist_open() {
        let checks = vec![
            Check::met("voxtype", "/usr/bin/voxtype".to_owned()),
            Check::advisory("speed", super::CPU_ONLY, fix()),
        ];
        assert!(all_met(&checks));
    }

    #[test]
    fn an_unmet_requirement_holds_it_open() {
        let checks = vec![Check::unmet("model", super::NO_MODEL, fix())];
        assert!(!all_met(&checks));
    }

    /// A probe that has not answered is not "ready": the checklist has to stay
    /// open and show the spinner, or the first frame of a launch claims
    /// readiness it has not established.
    #[test]
    fn a_pending_probe_holds_it_open() {
        let checks = vec![Check::waiting("audio sources")];
        assert!(!all_met(&checks));
        assert_eq!(checks[0].status, Status::Waiting);
    }
}
