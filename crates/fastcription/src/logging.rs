//! How much the app says on its standard error, and a way to change it while
//! it is running.
//!
//! The subscriber used to be built once at startup at `info` and left there,
//! which printed thirty lines of font-fallback bookkeeping on every launch
//! before a user had done anything. `info` is what a library author means by
//! "worth noting"; for someone who opened a terminal to run the app, it is
//! noise. The default is now `warn`, the level is a setting, and the filter is
//! behind a [`reload::Handle`] so the setting takes effect at once rather
//! than at the next launch.
//!
//! `RUST_LOG` still wins. It is the developer's override, set for one run, and
//! a setting that silently beat it would be the harder thing to debug.

use std::sync::OnceLock;

use tracing_subscriber::{reload, EnvFilter, Registry};

/// The live filter, once the subscriber is up.
static HANDLE: OnceLock<reload::Handle<EnvFilter, Registry>> = OnceLock::new();

/// Crates that narrate at `info` and are nobody's business at that level:
/// font fallback enumeration, GPU adapter selection, window-system plumbing.
/// Held to `warn` unless the user asks for `debug` or `trace`, at which point
/// they asked for everything and get it.
const QUIET: &[&str] = &[
    "fastframe_fonts",
    "wgpu_core",
    "wgpu_hal",
    "naga",
    "winit",
    "egui_wgpu",
];

/// How much to log. The order is the order of verbosity.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum LogLevel {
    Error,
    /// The default: something is wrong, or nothing is printed.
    #[default]
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    pub const ALL: [LogLevel; 5] = [
        LogLevel::Error,
        LogLevel::Warn,
        LogLevel::Info,
        LogLevel::Debug,
        LogLevel::Trace,
    ];

    pub fn label(self) -> &'static str {
        match self {
            LogLevel::Error => "Errors only",
            LogLevel::Warn => "Warnings",
            LogLevel::Info => "Information",
            LogLevel::Debug => "Debug",
            LogLevel::Trace => "Trace (everything)",
        }
    }

    fn name(self) -> &'static str {
        match self {
            LogLevel::Error => "error",
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
            LogLevel::Trace => "trace",
        }
    }

    /// The `EnvFilter` directives this level means.
    pub fn directives(self) -> String {
        match self {
            LogLevel::Debug | LogLevel::Trace => self.name().to_owned(),
            _ => {
                let quiet: Vec<String> = QUIET.iter().map(|c| format!("{c}=warn")).collect();
                format!("{},{}", self.name(), quiet.join(","))
            }
        }
    }
}

/// `RUST_LOG`, if it is set to anything.
pub fn env_override() -> Option<String> {
    std::env::var("RUST_LOG")
        .ok()
        .filter(|spec| !spec.trim().is_empty())
}

/// The filter to start with: `RUST_LOG` if set and parseable, else the
/// default level. The persisted setting is applied by the window once it has
/// read its settings, through [`apply`].
pub fn initial_filter() -> EnvFilter {
    env_override()
        .and_then(|spec| EnvFilter::try_new(spec).ok())
        .unwrap_or_else(|| EnvFilter::new(LogLevel::default().directives()))
}

/// Keeps the handle the subscriber was built with.
pub fn install(handle: reload::Handle<EnvFilter, Registry>) {
    // A second call would mean a second subscriber, which `init` refuses
    // anyway; the first handle stays.
    let _ = HANDLE.set(handle);
}

/// Changes the live level. Returns whether it did: `false` means `RUST_LOG`
/// is in force or no subscriber has been installed, and the caller can say
/// so.
pub fn apply(level: LogLevel) -> bool {
    if env_override().is_some() {
        return false;
    }
    let Some(handle) = HANDLE.get() else {
        return false;
    };
    handle.reload(EnvFilter::new(level.directives())).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole reason this module exists: a fresh install must not print
    /// font bookkeeping on launch.
    #[test]
    fn the_default_is_quiet() {
        assert_eq!(LogLevel::default(), LogLevel::Warn);
    }

    /// At the levels a user is likely to pick, the crates that narrate at
    /// `info` are held back; at the levels where they asked for everything,
    /// they are not.
    #[test]
    fn noisy_crates_are_quietened_unless_everything_was_asked_for() {
        for level in [LogLevel::Error, LogLevel::Warn, LogLevel::Info] {
            let d = level.directives();
            assert!(d.starts_with(level.name()), "{d}");
            assert!(d.contains("fastframe_fonts=warn"), "{d}");
        }
        for level in [LogLevel::Debug, LogLevel::Trace] {
            assert_eq!(level.directives(), level.name());
        }
    }

    /// Every directive string has to be something `EnvFilter` accepts, or the
    /// reload fails silently and the level never changes.
    #[test]
    fn every_level_parses_as_a_filter() {
        for level in LogLevel::ALL {
            assert!(
                EnvFilter::try_new(level.directives()).is_ok(),
                "{level:?}: {}",
                level.directives()
            );
        }
    }

    /// The setting is persisted, so each variant has to survive the settings
    /// file, and `ALL` is what the picker offers.
    #[test]
    fn levels_round_trip_and_are_all_offered() {
        for level in LogLevel::ALL {
            let json = serde_json::to_string(&level).unwrap();
            let back: LogLevel = serde_json::from_str(&json).unwrap();
            assert_eq!(level, back);
            assert!(!level.label().is_empty());
        }
        let mut seen = LogLevel::ALL.to_vec();
        seen.dedup();
        assert_eq!(seen.len(), LogLevel::ALL.len());
    }

    /// With no subscriber installed there is nothing to change, and that has
    /// to be reported rather than pretended.
    #[test]
    fn applying_with_no_subscriber_says_so() {
        if env_override().is_none() {
            assert!(!apply(LogLevel::Info));
        }
    }
}
