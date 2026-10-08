//! `cli.rs` parsers, against fixtures captured from the real voxtype 1.0.1
//! binary (`info engines`/`models`/`devices`/`accel`, `status --format json
//! --extended`), plus one hand-written fixture for the "fields missing"
//! case the real daemon did not happen to produce while these were captured.

use fc_voxtype::cli::{parse_accel, parse_devices, parse_engines, parse_models, StatusInfo};

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap_or_else(|e| panic!("reading fixture {name}: {e}"))
}

#[test]
fn engines_real_capture() {
    let entries = parse_engines(&fixture("info_engines.txt")).expect("parses");
    assert_eq!(entries.len(), 8);
    let whisper = &entries[0];
    assert_eq!(whisper.name, "whisper");
    assert!(whisper.compiled);
    assert!(whisper.active);
    let parakeet = &entries[1];
    assert_eq!(parakeet.name, "parakeet");
    assert!(!parakeet.compiled);
    assert!(!parakeet.active);
}

#[test]
fn engines_rejects_empty_output() {
    let err =
        parse_engines("Transcription engines\n\nSwitch with: voxtype config set engine <NAME>\n");
    assert!(err.is_err());
}

#[test]
fn models_real_capture() {
    let entries = parse_models(&fixture("info_models.txt")).expect("parses");
    // whisper.base.en is the only installed/default entry in the capture.
    let base_en = entries
        .iter()
        .find(|e| e.engine == "whisper" && e.name == "base.en")
        .expect("base.en present");
    assert!(base_en.installed);
    assert!(base_en.is_default);

    let tiny = entries
        .iter()
        .find(|e| e.engine == "whisper" && e.name == "tiny")
        .expect("tiny present");
    assert!(!tiny.installed);
    assert!(!tiny.is_default);

    let parakeet_default = entries
        .iter()
        .find(|e| e.engine == "parakeet" && e.name == "parakeet-tdt-0.6b-v3")
        .expect("parakeet default present");
    assert!(!parakeet_default.installed);
    assert!(parakeet_default.is_default);

    // Engines gated behind a download note still parse their rows.
    let dolphin = entries
        .iter()
        .find(|e| e.engine == "dolphin")
        .expect("dolphin present");
    assert_eq!(dolphin.name, "dolphin-base");
}

#[test]
fn devices_real_capture() {
    let entries = parse_devices(&fixture("info_devices.txt"));
    assert_eq!(entries.len(), 7);
    assert_eq!(entries[0].name, "default");
    assert!(entries[0].is_default);
    assert!(!entries[1].is_default);
    assert_eq!(entries[1].name, "pipewire");
}

#[test]
fn devices_empty_is_not_an_error() {
    let entries = parse_devices(
        "Audio input devices\n\nSelect one with: voxtype config set audio.device <NAME>\n",
    );
    assert!(entries.is_empty());
}

#[test]
fn accel_real_capture() {
    let info = parse_accel(&fixture("info_accel.txt")).expect("parses");
    assert_eq!(info.state.as_deref(), Some("cpu-only"));
    assert_eq!(info.backend.as_deref(), Some("(none in play)"));
    assert_eq!(info.variant.as_deref(), Some("avx2"));
    assert_eq!(info.daemon.as_deref(), Some("pid 2800853"));
}

/// An all-`None` `AccelInfo` is indistinguishable from a successful parse of
/// text that happened to say nothing, so output with no recognised key is an
/// error carrying what voxtype actually printed -- the same rule the engine and
/// model parsers follow.
#[test]
fn accel_rejects_output_with_no_recognised_key() {
    let err = parse_accel("GPU acceleration\n  something: else\n").unwrap_err();
    assert!(err.to_string().contains("something: else"), "{err}");
}

/// Individual keys do go missing for real: a CPU-only machine reports no
/// backend, and a daemon that is not running reports no pid.
#[test]
fn accel_keeps_parsing_when_only_some_keys_are_present() {
    let info = parse_accel("GPU acceleration\n  State: cpu-only\n").expect("parses");
    assert_eq!(info.state.as_deref(), Some("cpu-only"));
    assert_eq!(info.backend, None);
    assert_eq!(info.daemon, None);
}

#[test]
fn status_json_happy_path_real_capture() {
    let info: StatusInfo =
        serde_json::from_str(&fixture("status_json_happy.json")).expect("parses");
    assert_eq!(info.alt, "idle");
    assert_eq!(info.class, "idle");
    assert_eq!(info.model.as_deref(), Some("base.en"));
    assert_eq!(info.device.as_deref(), Some("default"));
    assert_eq!(info.backend.as_deref(), Some("CPU (AVX2)"));
}

#[test]
fn status_json_missing_fields_do_not_fail_the_parse() {
    let info: StatusInfo =
        serde_json::from_str(&fixture("status_json_missing_fields.json")).expect("parses");
    assert_eq!(info.alt, "recording");
    assert_eq!(info.model, None);
    assert_eq!(info.device, None);
    assert_eq!(info.backend, None);
}

#[test]
fn status_engine_info_fills_in_the_caller_supplied_engine_name() {
    let info: StatusInfo =
        serde_json::from_str(&fixture("status_json_happy.json")).expect("parses");
    let engine_info = info.engine_info("whisper", "en");
    assert_eq!(engine_info.engine, "whisper");
    assert_eq!(engine_info.model, "base.en");
    assert_eq!(engine_info.language, "en");
    assert_eq!(engine_info.backend.as_deref(), Some("CPU (AVX2)"));
}
