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
use std::time::Duration;

use fc_core::{AudioSource, CoreError, SourceKind};
use serde_json::Value;

use crate::{bounded, AudioError, Result};

/// `pactl` answers in milliseconds when the sound server is healthy, so five
/// seconds is not a budget any working call comes near — it is the line past
/// which the server is not going to answer at all. It has to exist because this
/// runs inside the capture thread on every reconnect, and on the interface
/// thread for the source picker.
const PACTL_TIMEOUT: Duration = Duration::from_secs(5);

/// Fields of a sink needed to resolve the default monitor.
struct SinkInfo {
    name: String,
    monitor_source: Option<String>,
}

fn run_pactl_json(args: &[&str]) -> Result<Value> {
    run_pactl_json_with("pactl", args, PACTL_TIMEOUT)
}

/// Split out so the timeout path can be tested against a deliberately slow fake
/// binary, without putting one on `PATH` — which, with cargo running tests in
/// one process, would mean mutating the environment under the other tests — and
/// without the test spending the real budget doing it.
fn run_pactl_json_with(binary: &str, args: &[&str], timeout: Duration) -> Result<Value> {
    let mut command = Command::new(binary);
    command.arg("-f").arg("json").args(args);

    let output =
        bounded::output_within(&mut command, timeout).map_err(|e| AudioError::Spawn("pactl", e))?;
    let Some(output) = output else {
        return Err(AudioError::Timeout {
            command: format!("{binary} -f json {}", args.join(" ")),
            after: timeout,
        });
    };
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
/// A sink-input's index does not survive the producing application restarting,
/// so a volatile source is matched against a fresh enumeration rather than
/// trusted as stored. A named source is confirmed to still exist. Either way, a
/// source that cannot be found is a clear, specific error — the caller surfaces
/// it to the user rather than silently recording the wrong thing.
///
/// This also runs on every capture reconnect, not only at session start, so
/// getting it wrong does not just pick the wrong stream once.
pub fn resolve(source: &AudioSource) -> Result<AudioSource> {
    if source.is_volatile() {
        return resolve_sink_input(
            source,
            &parse_sink_inputs(run_pactl_json(&["list", "sink-inputs"])?),
        );
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

/// Matches a stored sink-input descriptor against the live streams, most
/// specific first.
///
/// The order is the whole point. `application` alone is the *ambiguous* key —
/// a browser with a call in one tab and music in another is two streams with
/// the same `application.name` — while `description` carries `media.name` and is
/// what tells them apart. Trying `application` first therefore recorded
/// whichever stream `pactl` happened to list first, which is to say: whichever
/// one it felt like, on every reconnect. So: both keys, then the discriminating
/// one alone (the application was renamed), then the ambiguous one alone (the
/// stream's title changed, which happens whenever a meeting's subject does).
///
/// A tier that matches several streams is never resolved by picking one.
fn resolve_sink_input(source: &AudioSource, current: &[AudioSource]) -> Result<AudioSource> {
    let app = source.application.as_deref();
    let matches_app = |c: &AudioSource| app.is_some() && c.application.as_deref() == app;
    let matches_desc = |c: &AudioSource| c.description == source.description;

    let tiers: [Vec<&AudioSource>; 3] = [
        current
            .iter()
            .filter(|c| matches_app(c) && matches_desc(c))
            .collect(),
        current.iter().filter(|c| matches_desc(c)).collect(),
        current.iter().filter(|c| matches_app(c)).collect(),
    ];

    for tier in &tiers {
        if let [only] = tier[..] {
            return Ok(only.clone());
        }
    }

    let looked_for = format!(
        "application stream '{}' ({})",
        app.unwrap_or("unknown"),
        source.description
    );
    // The broadest tier that still matched more than one stream describes the
    // ambiguity best: it is the widest net that failed to catch a single thing.
    if let Some(tier) = tiers.iter().rev().find(|tier| tier.len() > 1) {
        return Err(AudioError::Ambiguous {
            looked_for,
            candidates: tier.iter().map(|c| candidate_label(c)).collect(),
        });
    }
    Err(AudioError::Core(CoreError::SourceGone(looked_for)))
}

/// A sink input named for a human choosing between two of them, so the index
/// is part of it — two streams of the same application can otherwise print
/// identically, which is exactly the case this is for.
fn candidate_label(source: &AudioSource) -> String {
    match source.index {
        Some(index) => format!("{} (#{index})", source.label()),
        None => source.label(),
    }
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
        // The capturing machine's user name, host name and machine id were
        // replaced with placeholders: a systemd machine id is a stable
        // per-installation identifier and does not belong in a public
        // repository. Nothing asserted here reads them.
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

    /// Two streams of the same application, told apart only by their
    /// descriptions: the stored one must come back, not whichever `pactl`
    /// listed first.
    #[test]
    fn an_exact_match_on_both_keys_wins_over_the_application_alone() {
        let current = vec![
            AudioSource::sink_input(900, "firefox", "firefox — Music"),
            AudioSource::sink_input(901, "firefox", "firefox — Team meeting"),
        ];
        let stored = AudioSource::sink_input(42, "firefox", "firefox — Team meeting");

        let resolved = resolve_sink_input(&stored, &current).expect("the exact match resolves");
        assert_eq!(resolved.index, Some(901));
        assert_eq!(resolved.description, "firefox — Team meeting");
    }

    /// The application was renamed (or reports itself differently after an
    /// update), but the stream title is unchanged: the description is the
    /// discriminating key, so it is tried before the application.
    #[test]
    fn the_description_resolves_when_the_application_name_changed() {
        let current = vec![AudioSource::sink_input(
            910,
            "Firefox",
            "firefox — Team meeting",
        )];
        let stored = AudioSource::sink_input(42, "firefox", "firefox — Team meeting");

        let resolved = resolve_sink_input(&stored, &current).expect("the description resolves");
        assert_eq!(resolved.index, Some(910));
    }

    /// The stream's title changed — which happens every time a meeting's
    /// subject does — so only the application still matches. One candidate, so
    /// it is unambiguous.
    #[test]
    fn the_application_resolves_when_it_is_the_only_stream_left() {
        let current = vec![
            AudioSource::sink_input(920, "Zoom", "Zoom — Another meeting"),
            AudioSource::sink_input(921, "firefox", "firefox — Music"),
        ];
        let stored = AudioSource::sink_input(42, "Zoom", "Zoom — Yesterday's meeting");

        let resolved = resolve_sink_input(&stored, &current).expect("the application resolves");
        assert_eq!(resolved.index, Some(920));
    }

    /// Nothing distinguishes the two candidates, so neither is recorded: half a
    /// meeting transcribed from the wrong stream is worse than being asked.
    #[test]
    fn two_streams_of_one_application_are_ambiguous_rather_than_guessed() {
        let current = vec![
            AudioSource::sink_input(930, "Zoom", "Zoom — Call"),
            AudioSource::sink_input(931, "Zoom", "Zoom — Screen share"),
        ];
        let stored = AudioSource::sink_input(42, "Zoom", "Zoom — Yesterday's meeting");

        let err = resolve_sink_input(&stored, &current).expect_err("must not pick one");
        match err {
            AudioError::Ambiguous {
                looked_for,
                candidates,
            } => {
                assert!(looked_for.contains("Zoom"), "{looked_for}");
                assert_eq!(candidates.len(), 2);
                assert!(
                    candidates.iter().any(|c| c.contains("#930"))
                        && candidates.iter().any(|c| c.contains("#931")),
                    "both candidates must be named so the user can choose: {candidates:?}"
                );
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn a_stream_that_is_gone_is_not_ambiguous_but_missing() {
        let current = vec![AudioSource::sink_input(940, "firefox", "firefox — Music")];
        let stored = AudioSource::sink_input(42, "Zoom", "Zoom — Call");

        let err = resolve_sink_input(&stored, &current).expect_err("nothing matches");
        assert!(
            matches!(err, AudioError::Core(CoreError::SourceGone(_))),
            "got {err:?}"
        );
    }

    /// A `pactl` that never answers must not block the caller: this runs on the
    /// interface thread for the picker and inside the capture thread on every
    /// reconnect, where it would otherwise hold up `Session::stop`.
    #[test]
    fn a_pactl_that_never_answers_times_out_instead_of_blocking() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script =
            crate::bounded::fake_binary(dir.path(), "slow-pactl", "#!/bin/sh\nexec sleep 30\n");

        // A shorter budget than production's `PACTL_TIMEOUT`: what is under test
        // is that a wedged `pactl` becomes an error rather than a wait, not the
        // value of the constant, and spending five seconds to say so would be
        // five seconds on every run of this suite.
        let budget = Duration::from_millis(200);
        let start = std::time::Instant::now();
        let err = super::run_pactl_json_with(
            script.to_str().expect("utf8 path"),
            &["list", "sources"],
            budget,
        )
        .expect_err("a wedged pactl must be an error, not a wait");
        match err {
            AudioError::Timeout { after, command } => {
                assert_eq!(after, budget);
                assert!(command.contains("list sources"), "{command}");
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
