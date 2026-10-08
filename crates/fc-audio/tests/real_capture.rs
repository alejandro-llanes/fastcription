//! Exercises real capture against the default monitor, with real audio.
//!
//! Playing a tone into the default sink and recording that sink's monitor is
//! the same path a user takes to transcribe a meeting, and it is the only way
//! this assertion can mean anything: a monitor with nothing playing produces
//! no samples at all, so a test that merely opened a capture and hoped for
//! audio would pass or fail depending on whether music happened to be on.
//!
//! Needs a running PipeWire/PulseAudio server and `paplay`. Run by hand:
//!
//! ```sh
//! cargo test -p fc-audio --test real_capture -- --ignored --nocapture
//! ```

use std::io::Write;
use std::time::Duration;

use crossbeam_channel::unbounded;
use fc_audio::{levels, sources, CaptureBackend, ParecCapture};

const SAMPLE_RATE: u32 = 48_000;
const TONE_HZ: f32 = 440.0;
const SECONDS: u32 = 2;

#[test]
#[ignore = "needs a sound server and paplay"]
fn captures_a_tone_played_into_the_default_sink() {
    let wav = std::env::temp_dir().join("fc-audio-capture-probe.wav");
    write_tone(&wav).expect("write the probe tone");

    let source = sources::default_source()
        .expect("enumerate for a default source")
        .expect("a default monitor to capture");
    println!("capturing {}", source.label());

    let (pcm_tx, pcm_rx) = unbounded();
    let (event_tx, event_rx) = unbounded();
    let mut capture = ParecCapture::start(source, pcm_tx, event_tx);

    // parec needs a moment to connect before there is anything to miss.
    std::thread::sleep(Duration::from_millis(700));
    let played = std::process::Command::new("paplay")
        .arg(&wav)
        .status()
        .expect("run paplay");
    assert!(
        played.success(),
        "paplay could not play into the default sink"
    );
    std::thread::sleep(Duration::from_millis(300));

    capture.stop();
    let _ = std::fs::remove_file(&wav);

    let captured: Vec<f32> = pcm_rx.try_iter().flatten().collect();
    let events: Vec<_> = event_rx.try_iter().collect();
    println!(
        "captured {} samples, {} events",
        captured.len(),
        events.len()
    );

    assert!(
        !captured.is_empty(),
        "no samples arrived from the monitor; is the default sink the one paplay used?"
    );
    // A tone at full scale must come back as something, not as silence: this is
    // what proves the i16-to-f32 decode is wired up and not producing zeros.
    let peak = levels::peak(&captured);
    assert!(
        peak > 0.05,
        "captured {} samples but their peak was {peak}, which is silence",
        captured.len()
    );
}

/// A mono 16-bit WAV of a sine tone, written without a WAV crate so this test
/// pulls in no dependency the crate does not already have.
fn write_tone(path: &std::path::Path) -> std::io::Result<()> {
    let frames = SAMPLE_RATE * SECONDS;
    let data_len = frames * 2;
    let mut file = std::fs::File::create(path)?;

    file.write_all(b"RIFF")?;
    file.write_all(&(36 + data_len).to_le_bytes())?;
    file.write_all(b"WAVEfmt ")?;
    file.write_all(&16u32.to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?; // PCM
    file.write_all(&1u16.to_le_bytes())?; // mono
    file.write_all(&SAMPLE_RATE.to_le_bytes())?;
    file.write_all(&(SAMPLE_RATE * 2).to_le_bytes())?; // byte rate
    file.write_all(&2u16.to_le_bytes())?; // block align
    file.write_all(&16u16.to_le_bytes())?; // bits per sample
    file.write_all(b"data")?;
    file.write_all(&data_len.to_le_bytes())?;

    for frame in 0..frames {
        let phase = frame as f32 / SAMPLE_RATE as f32 * TONE_HZ * std::f32::consts::TAU;
        let sample = (phase.sin() * 20_000.0) as i16;
        file.write_all(&sample.to_le_bytes())?;
    }
    file.flush()
}
