//! Rendering an instant for a human.
//!
//! This lives in `fc-core` because both the views and the exporters need it:
//! `started_at`/`ended_at` are stored as bare integers (see [`UnixMillis`]),
//! and without a shared formatter every caller either invents its own or —
//! as happened here — shows nothing at all.
//!
//! The local UTC offset is read per call and falls back to UTC. `time` refuses
//! to determine it in a process that has already started threads, which
//! fastcription always has (architecture §3), so a wrong-by-an-offset
//! timestamp is the realistic failure and silently showing UTC beats showing
//! an error where a date belongs.

use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

use crate::UnixMillis;

/// `YYYY-MM-DD HH:MM` in local time: a conversation list row needs the day
/// more than it needs the second.
pub fn local_datetime(millis: UnixMillis) -> String {
    let format = time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]");
    render(millis, format)
}

/// `HH:MM` in local time, for a row whose date is already established by its
/// surroundings.
pub fn local_time(millis: UnixMillis) -> String {
    let format = time::macros::format_description!("[hour]:[minute]");
    render(millis, format)
}

/// RFC 3339 UTC, for metadata headers and anything meant to be parsed again.
/// Truncated to the second: a transcript's start is not a measurement.
pub fn rfc3339(millis: UnixMillis) -> String {
    match at(millis) {
        Some(dt) => dt.format(&Rfc3339).unwrap_or_else(|_| millis.to_string()),
        None => millis.to_string(),
    }
}

/// `m:ss`, growing to `h:mm:ss` only once there is an hour to show. A
/// duration is read at a glance, so leading zeros that carry no information
/// are left out.
pub fn duration_hms(ms: u64) -> String {
    let seconds = ms / 1000;
    let (hours, minutes, seconds) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// Falls back to the raw integer rather than panicking or returning an empty
/// string: an instant outside `time`'s range means a corrupt row, and showing
/// the number is what lets someone recognise that.
fn render(millis: UnixMillis, format: &[time::format_description::FormatItem<'_>]) -> String {
    match at(millis) {
        Some(dt) => dt
            .to_offset(UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC))
            .format(format)
            .unwrap_or_else(|_| millis.to_string()),
        None => millis.to_string(),
    }
}

/// `div_euclid` rather than `/` so an instant before 1970 floors instead of
/// rounding towards zero, which would place it in the wrong second.
fn at(millis: UnixMillis) -> Option<OffsetDateTime> {
    OffsetDateTime::from_unix_timestamp(millis.div_euclid(1000)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2025-10-09T08:53:20Z, the instant the export fixtures use.
    const SAMPLE: UnixMillis = 1_760_000_000_000;

    #[test]
    fn rfc3339_is_utc_to_the_second() {
        assert_eq!(rfc3339(SAMPLE), "2025-10-09T08:53:20Z");
        assert_eq!(rfc3339(SAMPLE + 500), "2025-10-09T08:53:20Z");
    }

    #[test]
    fn local_datetime_and_time_agree_with_the_offset_in_force() {
        let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
        let expected = OffsetDateTime::from_unix_timestamp(SAMPLE / 1000)
            .unwrap()
            .to_offset(offset);
        assert_eq!(
            local_datetime(SAMPLE),
            format!(
                "{:04}-{:02}-{:02} {:02}:{:02}",
                expected.year(),
                u8::from(expected.month()),
                expected.day(),
                expected.hour(),
                expected.minute()
            )
        );
        assert_eq!(
            local_time(SAMPLE),
            format!("{:02}:{:02}", expected.hour(), expected.minute())
        );
    }

    #[test]
    fn an_unrepresentable_instant_shows_its_number_instead_of_panicking() {
        assert_eq!(local_datetime(i64::MIN), i64::MIN.to_string());
        assert_eq!(local_time(i64::MIN), i64::MIN.to_string());
        assert_eq!(rfc3339(i64::MAX), i64::MAX.to_string());
    }

    #[test]
    fn duration_drops_the_hour_until_there_is_one() {
        assert_eq!(duration_hms(0), "0:00");
        assert_eq!(duration_hms(9_999), "0:09");
        assert_eq!(duration_hms(90_000), "1:30");
        assert_eq!(duration_hms(59 * 60 * 1000 + 59_000), "59:59");
        assert_eq!(duration_hms(60 * 60 * 1000), "1:00:00");
        assert_eq!(duration_hms(90 * 60 * 1000 + 4_000), "1:30:04");
    }
}
