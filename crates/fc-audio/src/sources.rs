//! Enumeration of capture targets via `pactl -f json`.
//!
//! There is no `monitor_of_sink` key in the JSON this machine's `pactl`
//! produces for a source, even for a source that plainly is a monitor —
//! confirmed by capturing real output rather than trusting the documented
//! shape. `properties."device.class" == "monitor"` is what actually carries
//! that fact here, so it is the primary signal; the documented
//! `monitor_of_sink` field (string `"n/a"`, `null`, or a sink index) is kept as
//! a secondary signal for `pactl` builds that do emit it. Every other field is
//! read defensively: a missing or unexpected value drops that one entry rather
//! than failing the whole enumeration, because one malformed stream must never
//! hide every other source from the picker.

use std::process::Command;

use fc_core::{AudioSource, CoreError, SourceKind};
use serde_json::Value;

use crate::{AudioError, Result};

/// Fields of a sink needed to resolve the default monitor.
struct SinkInfo {
    name: String,
    monitor_source: Option<String>,
}

fn run_pactl_json(args: &[&str]) -> Result<Value> {
    let output = Command::new("pactl")
        .arg("-f")
        .arg("json")
        .args(args)
        .output()
        .map_err(|e| AudioError::Spawn("pactl", e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AudioError::CommandFailed(
            "pactl",
            if stderr.is_empty() {
                output.status.to_string()
            } else {
                stderr
            },
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(serde_json::from_str(&text)?)
}

fn as_array(value: Value) -> Vec<Value> {
    match value {
        Value::Array(items) => items,
        _ => Vec::new(),
    }
}

/// `true` when a `monitor_of_sink`-shaped field asserts that its source is a
/// monitor: present, not `null`, and not the textual `"n/a"`.
fn field_says_monitor(field: Option<&Value>) -> bool {
    match field {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) if s == "n/a" => false,
        Some(_) => true,
    }
}

fn is_monitor_source(entry: &Value) -> bool {
    let device_class = entry
        .get("properties")
        .and_then(|p| p.get("device.class"))
        .and_then(Value::as_str);
    device_class == Some("monitor") || field_says_monitor(entry.get("monitor_of_sink"))
}

fn classify_source(entry: &Value) -> Option<AudioSource> {
    let name = entry.get("name")?.as_str()?.to_string();
    let description = entry
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or(&name)
        .to_string();
    let kind = if is_monitor_source(entry) {
        SourceKind::SinkMonitor
    } else {
        SourceKind::Device
    };
    Some(AudioSource::named(kind, name, description))
}

fn classify_sink_input(entry: &Value) -> Option<AudioSource> {
    let index = entry.get("index")?.as_u64()? as u32;
    let props = entry.get("properties")?;
    let app_name = props.get("application.name").and_then(Value::as_str);
    let process_binary = props
        .get("application.process.binary")
        .and_then(Value::as_str);
    let media_name = props.get("media.name").and_then(Value::as_str);

    let identity = app_name.or(process_binary).or(media_name)?;
    if identity.eq_ignore_ascii_case("fastcription") {
        return None;
    }

    let description = match media_name {
        Some(m) if !m.eq_ignore_ascii_case(identity) => format!("{identity} — {m}"),
        _ => identity.to_string(),
    };
    Some(AudioSource::sink_input(index, identity, description))
}

fn parse_sources(raw: Value) -> Vec<AudioSource> {
    let entries = as_array(raw);
    let sources: Vec<AudioSource> = entries.iter().filter_map(classify_source).collect();
    report_skipped("source", entries.len(), sources.len());
    sources
}

fn parse_sink_inputs(raw: Value) -> Vec<AudioSource> {
    let entries = as_array(raw);
    let inputs: Vec<AudioSource> = entries.iter().filter_map(classify_sink_input).collect();
    report_skipped("sink input", entries.len(), inputs.len());
    inputs
}

/// A dropped entry is deliberate — one malformed stream must not hide every
/// other source from the picker — but silence about it makes a missing device
/// impossible to explain, so the count is logged.
fn report_skipped(kind: &str, seen: usize, kept: usize) {
    let skipped = seen.saturating_sub(kept);
    if skipped > 0 {
        tracing::debug!(kind, skipped, seen, "skipped unusable pactl entries");
    }
}

fn parse_sinks(raw: Value) -> Vec<SinkInfo> {
    as_array(raw)
        .iter()
        .filter_map(|entry| {
            let name = entry.get("name")?.as_str()?.to_string();
            let monitor_source = entry
                .get("monitor_source")
                .and_then(Value::as_str)
                .map(str::to_string);
            Some(SinkInfo {
                name,
                monitor_source,
            })
        })
        .collect()
}

/// Every source and sink-input this machine currently offers for capture.
///
/// Sink monitors come back as part of `list sources` (see the module docs);
/// `list sinks` itself is not used here, only by [`default_source`].
pub fn enumerate() -> Result<Vec<AudioSource>> {
    let mut out = parse_sources(run_pactl_json(&["list", "sources"])?);
    out.extend(parse_sink_inputs(run_pactl_json(&["list", "sink-inputs"])?));
    Ok(out)
}

/// Re-resolves `source` against what `pactl` reports right now.
///
/// A sink-input's index does not survive the producing application
/// restarting, so a volatile source is matched by application name (falling
/// back to its description) against a fresh enumeration instead of trusted as
/// stored. A named source is confirmed to still exist. Either way, a source
/// that cannot be found is a clear, specific error — the caller surfaces it to
/// the user rather than silently recording the wrong thing.
pub fn resolve(source: &AudioSource) -> Result<AudioSource> {
    if source.is_volatile() {
        let current = parse_sink_inputs(run_pactl_json(&["list", "sink-inputs"])?);
        let app = source.application.as_deref();
        if let Some(found) = current
            .iter()
            .find(|c| app.is_some() && c.application.as_deref() == app)
        {
            return Ok(found.clone());
        }
        if let Some(found) = current.iter().find(|c| c.description == source.description) {
            return Ok(found.clone());
        }
        return Err(AudioError::Core(CoreError::SourceGone(format!(
            "application stream '{}' ({})",
            app.unwrap_or("unknown"),
            source.description
        ))));
    }

    let current = parse_sources(run_pactl_json(&["list", "sources"])?);
    if let Some(found) = current.iter().find(|c| c.name == source.name) {
        return Ok(found.clone());
    }
    Err(AudioError::Core(CoreError::SourceGone(format!(
        "source '{}' ({})",
        source.name, source.description
    ))))
}

/// The default sink's monitor, as a sensible first suggestion for
/// transcribing a meeting (everything the system is playing). `None` when
/// there is no default sink or its monitor is not among the enumerated
/// sources.
pub fn default_source() -> Result<Option<AudioSource>> {
    let info = run_pactl_json(&["info"])?;
    let Some(default_sink) = info.get("default_sink_name").and_then(Value::as_str) else {
        return Ok(None);
    };

    let sinks = parse_sinks(run_pactl_json(&["list", "sinks"])?);
    let Some(monitor_name) = sinks
        .iter()
        .find(|s| s.name == default_sink)
        .and_then(|s| s.monitor_source.clone())
    else {
        return Ok(None);
    };

    let sources = parse_sources(run_pactl_json(&["list", "sources"])?);
    Ok(sources.into_iter().find(|s| s.name == monitor_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(path).expect("fixture file");
        serde_json::from_str(&text).expect("fixture json")
    }

    #[test]
    fn real_machine_source_is_classified_as_a_monitor() {
        let sources = parse_sources(fixture("sources_real.json"));
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].kind, SourceKind::SinkMonitor);
        assert_eq!(sources[0].name, "auto_null.monitor");
        assert_eq!(sources[0].description, "Monitor of Dummy Output");
    }

    #[test]
    fn real_machine_has_no_sink_inputs() {
        let inputs = parse_sink_inputs(fixture("sink_inputs_real.json"));
        assert!(inputs.is_empty());
    }

    #[test]
    fn real_machine_sink_input_from_paplay() {
        // Captured while `paplay` was actively streaming into `auto_null`.
        let inputs = parse_sink_inputs(fixture("sink_inputs_real_paplay.json"));
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].kind, SourceKind::SinkInput);
        assert_eq!(inputs[0].index, Some(95864));
        assert_eq!(inputs[0].application.as_deref(), Some("paplay"));
    }

    #[test]
    fn handwritten_sources_cover_both_monitor_signals_and_both_non_monitor_markers() {
        let sources = parse_sources(fixture("sources_handwritten.json"));
        // Entry 5 has no `name` and must be skipped, not abort the parse.
        assert_eq!(sources.len(), 4);

        let by_name = |name: &str| sources.iter().find(|s| s.name == name).unwrap();

        // device.class == "monitor" with no monitor_of_sink key at all (this
        // machine's real shape).
        assert_eq!(
            by_name("alsa_output.usb-Blue_Microphones_Yeti-00.analog-stereo.monitor").kind,
            SourceKind::SinkMonitor
        );
        // An explicit, present, non-null monitor_of_sink (a pactl build that
        // does emit it), with no device.class at all.
        assert_eq!(
            by_name("alsa_output.pci-0000_00_1f.3.analog-stereo.monitor").kind,
            SourceKind::SinkMonitor
        );
        // monitor_of_sink: "n/a" must read as "not a monitor".
        assert_eq!(
            by_name("alsa_input.pci-0000_00_1f.3.analog-stereo").kind,
            SourceKind::Device
        );
        // monitor_of_sink: null must also read as "not a monitor".
        assert_eq!(
            by_name("alsa_input.usb-Blue_Microphones_Yeti-00.analog-stereo").kind,
            SourceKind::Device
        );
    }

    #[test]
    fn handwritten_sink_inputs_cover_identity_fallback_and_filtering() {
        let inputs = parse_sink_inputs(fixture("sink_inputs_handwritten.json"));
        // #152 has no usable identity and is skipped; #153 is our own stream
        // and is filtered out.
        assert_eq!(inputs.len(), 2);

        let zoom = &inputs[0];
        assert_eq!(zoom.index, Some(150));
        assert_eq!(zoom.application.as_deref(), Some("Zoom"));
        assert_eq!(zoom.description, "Zoom — Zoom Meeting");

        let firefox = &inputs[1];
        assert_eq!(firefox.index, Some(151));
        assert_eq!(firefox.application.as_deref(), Some("firefox"));
        assert_eq!(firefox.description, "firefox — Playback");
    }

    #[test]
    fn sinks_fixture_maps_default_sink_to_its_monitor_name() {
        let sinks = parse_sinks(fixture("sinks_real.json"));
        assert_eq!(sinks.len(), 1);
        assert_eq!(sinks[0].name, "auto_null");
        assert_eq!(
            sinks[0].monitor_source.as_deref(),
            Some("auto_null.monitor")
        );
    }
}
