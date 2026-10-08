//! Timestamp formatting, built on `time::Duration` rather than hand-rolled
//! division so the hour/minute/second split can't drift.
//!
//! `Segment::start_ms`/`end_ms` are offsets from the start of the
//! conversation (see `fc_core::transcript`), not wall-clock instants, so
//! these never wrap at 24:00:00 the way a `time::Time` would -- a long
//! enough meeting legitimately produces an `HH` past 23.

use time::Duration;

fn split(ms: u64) -> (i64, i64, i64, i64) {
    let d = Duration::milliseconds(ms as i64);
    let hours = d.whole_hours();
    let minutes = d.whole_minutes() - hours * 60;
    let seconds = d.whole_seconds() - d.whole_minutes() * 60;
    let millis = d.whole_milliseconds() - d.whole_seconds() as i128 * 1000;
    (hours, minutes, seconds, millis as i64)
}

/// `HH:MM:SS,mmm`, the SRT cue timestamp.
pub(crate) fn srt(ms: u64) -> String {
    let (h, m, s, ms) = split(ms);
    format!("{h:02}:{m:02}:{s:02},{ms:03}")
}

/// `HH:MM:SS.mmm`, the WebVTT cue timestamp.
pub(crate) fn vtt(ms: u64) -> String {
    let (h, m, s, ms) = split(ms);
    format!("{h:02}:{m:02}:{s:02}.{ms:03}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srt_format() {
        assert_eq!(srt(0), "00:00:00,000");
        assert_eq!(srt(1_234), "00:00:01,234");
    }

    #[test]
    fn ninety_minutes_rolls_into_hh() {
        assert_eq!(srt(90 * 60 * 1000), "01:30:00,000");
        assert_eq!(vtt(90 * 60 * 1000), "01:30:00.000");
    }

    #[test]
    fn vtt_uses_a_dot_not_a_comma() {
        assert_eq!(vtt(500), "00:00:00.500");
    }
}
