//! Exercises real capture against the default monitor. Requires an actual
//! PipeWire/PulseAudio server with a recordable source — this dev machine has
//! only `auto_null`, so run by hand on a machine with real audio devices:
//!
//! ```sh
//! cargo test -p fc-audio --test real_capture -- --ignored --nocapture
//! ```

use std::time::Duration;

use crossbeam_channel::unbounded;
use fc_audio::{sources, CaptureBackend, ParecCapture};

#[test]
#[ignore]
fn captures_a_second_of_the_default_monitor() {
    let source = sources::default_source()
        .expect("enumerate for a default source")
        .expect("a default monitor to capture");

    let (pcm_tx, pcm_rx) = unbounded();
    let (event_tx, event_rx) = unbounded();

    let mut capture = ParecCapture::start(source, pcm_tx, event_tx);
    std::thread::sleep(Duration::from_secs(1));
    capture.stop();

    let frames: usize = pcm_rx.try_iter().map(|f| f.len()).sum();
    let events: Vec<_> = event_rx.try_iter().collect();
    println!("captured {frames} samples, {} events", events.len());
    assert!(frames > 0, "expected at least one decoded sample");
}
