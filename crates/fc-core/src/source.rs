//! The audio source the user picked, in a form that survives a restart.
//!
//! fastcription transcribes exactly one source by choice, so which source it is
//! has to be recorded precisely and resolved again later. PipeWire source names
//! are stable; sink-input indices are not, which is why an [`AudioSource`]
//! carries enough description to be matched again by other means.

use serde::{Deserialize, Serialize};

/// What kind of thing is being captured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SourceKind {
    /// A real capture device: a microphone, a line input, a virtual source.
    Device,
    /// The monitor of an output sink: everything the system plays through it.
    SinkMonitor,
    /// One application's playback stream, captured on its own. The most useful
    /// option for a meeting, and the only volatile one.
    SinkInput,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Device => "device",
            Self::SinkMonitor => "sink-monitor",
            Self::SinkInput => "sink-input",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "device" => Some(Self::Device),
            "sink-monitor" => Some(Self::SinkMonitor),
            "sink-input" => Some(Self::SinkInput),
            _ => None,
        }
    }
}

/// A capture source, as chosen by the user and as stored with a conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioSource {
    pub kind: SourceKind,
    /// PipeWire/PulseAudio source name, e.g. `alsa_output.pci-0000_00_1f.3.analog-stereo.monitor`.
    /// Empty for [`SourceKind::SinkInput`], which has no source name of its own.
    pub name: String,
    /// Human-readable label for the picker and for the conversation record.
    pub description: String,
    /// `application.name` of the stream, for a sink input. This is what makes a
    /// volatile index re-resolvable after the application restarts.
    pub application: Option<String>,
    /// Sink-input index. Valid only for the lifetime of that stream, so it is
    /// never trusted on load without re-resolving first.
    pub index: Option<u32>,
}

impl AudioSource {
    /// A named source: a device or a sink monitor.
    pub fn named(kind: SourceKind, name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            kind,
            name: name.into(),
            description: description.into(),
            application: None,
            index: None,
        }
    }

    /// One application's playback stream.
    pub fn sink_input(
        index: u32,
        application: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            kind: SourceKind::SinkInput,
            name: String::new(),
            description: description.into(),
            application: Some(application.into()),
            index: Some(index),
        }
    }

    /// True when the identifier can go stale while the app is not running, so
    /// it must be resolved against the live server before use.
    pub fn is_volatile(&self) -> bool {
        self.kind == SourceKind::SinkInput
    }

    /// The `parec` arguments that capture this source.
    ///
    /// Panics are impossible here: a sink input without an index cannot be
    /// constructed through the public API, and the fallback keeps a
    /// hand-deserialised value from producing a command that records the wrong
    /// thing — it records nothing instead.
    pub fn parec_target(&self) -> Option<Vec<String>> {
        match self.kind {
            SourceKind::Device | SourceKind::SinkMonitor => {
                if self.name.is_empty() {
                    None
                } else {
                    Some(vec!["--device".into(), self.name.clone()])
                }
            }
            SourceKind::SinkInput => self
                .index
                .map(|i| vec![format!("--monitor-stream={i}")]),
        }
    }

    /// What to show in a list: the description, with the application in front
    /// when that is what distinguishes it.
    pub fn label(&self) -> String {
        match (&self.application, self.description.is_empty()) {
            (Some(app), false) => format!("{app} — {}", self.description),
            (Some(app), true) => app.clone(),
            (None, false) => self.description.clone(),
            (None, true) => self.name.clone(),
        }
    }
}
