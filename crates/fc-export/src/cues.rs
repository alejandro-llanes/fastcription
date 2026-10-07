//! Splits one transcript segment into one or more subtitle cues.
//!
//! A segment can run much longer than a cue should stay on screen (the
//! segmenter's own chunks run up to 15s of audio per ARCHITECTURE.md §3, and
//! a single committed segment can carry a full chunk of speech as one block
//! of text). [`split`] breaks long text on word boundaries and spreads the
//! segment's duration across the pieces proportionally to their length, so
//! cues stay roughly in sync with speech rate without needing word-level
//! timing voxtype does not give us.

/// Cues longer than this (in characters) get split. Chosen to match the
/// common subtitling guideline of two ~42-character lines.
const MAX_CUE_CHARS: usize = 84;

/// Splits `text` (already trimmed) into `(start_ms, end_ms, text)` cues
/// covering `[start_ms, end_ms]`. `end_ms` is clamped to `start_ms` first, so
/// an out-of-order segment (`end_ms < start_ms`) still yields cues with
/// `end >= start`. Empty text yields no cues at all -- callers are expected
/// to have already skipped blank segments, but this stays safe either way.
pub(crate) fn split(start_ms: u64, end_ms: u64, text: &str) -> Vec<(u64, u64, String)> {
    let end_ms = end_ms.max(start_ms);
    if text.is_empty() {
        return Vec::new();
    }
    if text.chars().count() <= MAX_CUE_CHARS {
        return vec![(start_ms, end_ms, text.to_string())];
    }

    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let candidate_len = if current.is_empty() {
            word.chars().count()
        } else {
            current.chars().count() + 1 + word.chars().count()
        };
        if candidate_len > MAX_CUE_CHARS && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    if chunks.is_empty() {
        return Vec::new();
    }

    let total_chars: u64 = chunks.iter().map(|c| c.chars().count() as u64).sum::<u64>().max(1);
    let total_duration = end_ms.saturating_sub(start_ms);
    let mut cursor = start_ms;
    let last = chunks.len() - 1;
    chunks
        .into_iter()
        .enumerate()
        .map(|(i, chunk)| {
            let share = chunk.chars().count() as u64 * total_duration / total_chars;
            let chunk_end = if i == last { end_ms } else { (cursor + share).min(end_ms) };
            let chunk_end = chunk_end.max(cursor);
            let cue = (cursor, chunk_end, chunk);
            cursor = chunk_end;
            cue
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_one_cue() {
        let cues = split(0, 1000, "hello there");
        assert_eq!(cues, vec![(0, 1000, "hello there".to_string())]);
    }

    #[test]
    fn out_of_order_segment_still_yields_end_gte_start() {
        let cues = split(5_000, 1_000, "oops");
        assert_eq!(cues, vec![(5_000, 5_000, "oops".to_string())]);
    }

    #[test]
    fn zero_duration_segment_yields_a_single_zero_length_cue() {
        let cues = split(1_000, 1_000, "mm-hm");
        assert_eq!(cues, vec![(1_000, 1_000, "mm-hm".to_string())]);
    }

    #[test]
    fn long_text_splits_and_stays_monotonic() {
        let long = "one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty";
        let cues = split(0, 20_000, long);
        assert!(cues.len() > 1);
        let mut prev_end = 0u64;
        for (start, end, text) in &cues {
            assert!(*start >= prev_end || *start == 0);
            assert!(end >= start);
            assert!(text.chars().count() <= MAX_CUE_CHARS);
            prev_end = *end;
        }
        assert_eq!(cues.last().unwrap().1, 20_000);
    }

    #[test]
    fn empty_text_yields_no_cues() {
        assert!(split(0, 1000, "").is_empty());
    }
}
