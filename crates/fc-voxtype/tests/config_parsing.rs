//! `config.rs` against real files on this machine (the annotated default at
//! `/etc/voxtype/config.toml` and this user's own `~/.config/voxtype/config.toml`,
//! both copied in verbatim) plus hand-written empty/minimal/meeting-enabled
//! fixtures for shapes neither real file happens to exercise.

use fc_voxtype::config::parse_defaults;

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap_or_else(|e| panic!("reading fixture {name}: {e}"))
}

#[test]
fn empty_file_is_all_defaults() {
    let d = parse_defaults(&fixture("config_empty.toml")).expect("parses");
    assert_eq!(d, Default::default());
}

#[test]
fn minimal_file_fills_in_only_what_it_sets() {
    let d = parse_defaults(&fixture("config_minimal.toml")).expect("parses");
    assert_eq!(d.model.as_deref(), Some("small.en"));
    assert_eq!(d.language.as_deref(), Some("en"));
    assert_eq!(d.engine, None);
    assert_eq!(d.audio_device, None);
    assert_eq!(d.meeting, None);
}

#[test]
fn annotated_default_config_toml_real_file() {
    // /etc/voxtype/config.toml: every setting this module reads is commented
    // out except the ones voxtype itself ships uncommented.
    let d = parse_defaults(&fixture("config_annotated_default.toml")).expect("parses");
    assert_eq!(d.model.as_deref(), Some("base.en"));
    assert_eq!(d.language.as_deref(), Some("en"));
    assert_eq!(d.audio_device.as_deref(), Some("default"));
    // No [meeting] table at all in the shipped default: meeting mode is
    // opt-in, and this module must not invent one.
    assert_eq!(d.meeting, None);
}

#[test]
fn user_real_config_toml_real_file() {
    let d = parse_defaults(&fixture("config_user_real.toml")).expect("parses");
    assert_eq!(d.model.as_deref(), Some("base.en"));
    assert_eq!(d.language.as_deref(), Some("en"));
    assert_eq!(d.audio_device.as_deref(), Some("default"));
    assert_eq!(d.meeting, None);
}

#[test]
fn meeting_section_when_present() {
    let d = parse_defaults(&fixture("config_with_meeting.toml")).expect("parses");
    assert_eq!(d.engine.as_deref(), Some("parakeet"));
    // whisper.language as an array collapses to a comma-joined display string.
    assert_eq!(d.language.as_deref(), Some("en,fr"));
    assert_eq!(d.audio_device.as_deref(), Some("pipewire"));
    let meeting = d.meeting.expect("meeting section present");
    assert_eq!(meeting.enabled, Some(true));
    assert_eq!(meeting.chunk_duration_secs, Some(45));
    assert_eq!(
        meeting.storage_path.as_deref(),
        Some("/home/user/voxtype-meetings")
    );
    assert_eq!(meeting.mic_device.as_deref(), Some("default"));
    assert_eq!(meeting.loopback_device.as_deref(), Some("disabled"));
    assert_eq!(meeting.diarization_backend.as_deref(), Some("ml"));
}

#[test]
fn unknown_keys_and_sections_are_ignored() {
    let text = r#"
        engine = "whisper"
        totally_unknown_top_level_key = true

        [whisper]
        model = "base.en"
        language = "en"
        some_future_field = 42

        [a_section_this_crate_has_never_heard_of]
        anything = "goes"
    "#;
    let d = parse_defaults(text).expect("unknown keys must not fail the parse");
    assert_eq!(d.engine.as_deref(), Some("whisper"));
    assert_eq!(d.model.as_deref(), Some("base.en"));
}
