//! Splits one transcript segment into one or more subtitle cues.
//!
//! A segment can run much longer than a cue should stay on screen (a single
//! committed segment carries a whole finalised utterance per ARCHITECTURE.md
//! §3). [`split`] breaks long text on word boundaries and spreads the
//! segment's duration across the pieces proportionally to their length, so
//! cues stay roughly in sync with speech rate without needing word-level
//! timing voxtype does not give us.

/// Cues longer than this (in characters) get split. Chosen to match the
/// common subtitling guideline of two ~42-character lines.
const MAX_CUE_CHARS: usize = 84;

/// Shortest cue this will emit.
///
/// WebVTT requires `end > start`, and a zero-duration segment — which the
/// transcriber does produce for a one-word utterance — would otherwise stack
/// every one of its cues on the same instant, where no player shows any of
/// them. A floor is also simply readable: a line needs a moment on screen
/// regardless of what the timing claims.
const MIN_CUE_MS: u64 = 1_200;

/// Splits `text` into `(start_ms, end_ms, text)` cues covering
/// `[start_ms, end_ms]`.
///
/// `end_ms` is clamped to `start_ms` first, so an out-of-order segment
/// (`end_ms < start_ms`) still yields cues with `end > start`. Empty text
/// yields no cues at all -- callers are expected to have already skipped
/// blank segments, but this stays safe either way. Cues are monotonic: each
/// one starts where the previous ended.
pub(crate) fn split(start_ms: u64, end_ms: u64, text: &str) -> Vec<(u64, u64, String)> {
    let end_ms = end_ms.max(start_ms);
    // `Segment::text` is one line by invariant and the store enforces it, but
    // a blank line here would terminate the cue early and silently push the
    // rest of its text into the file as if it were cue syntax. Cheap to be
    // certain.
    let text = fc_core::single_line(text);
    let chunks = chunk(&text);
    if chunks.is_empty() {
        return Vec::new();
    }

    // `u128`: an imported meeting's `endMs` can be large enough that
    // `chars * duration` overflows `u64` (a 100-character cue does it past
    // about 1.8e17 ms), and the product is an intermediate nobody reads.
    let total_chars: u128 = chunks.iter().map(|c| c.chars().count() as u128).sum();
    let total_chars = total_chars.max(1);
    let total_duration = end_ms - start_ms;

    let mut cursor = start_ms;
    let last = chunks.len() - 1;
    let mut cues = Vec::with_capacity(chunks.len());
    for (i, chunk) in chunks.into_iter().enumerate() {
        let end = if total_duration == 0 {
            // No duration to share out: lay the cues back to back at the
            // minimum readable length instead of piling them on one instant.
            cursor.saturating_add(MIN_CUE_MS)
        } else if i == last {
            end_ms.max(cursor.saturating_add(1))
        } else {
            let share =
                (chunk.chars().count() as u128 * total_duration as u128 / total_chars) as u64;
            cursor
                .saturating_add(share)
                .min(end_ms)
                .max(cursor.saturating_add(1))
        };
        cues.push((cursor, end, chunk));
        cursor = end;
    }
    cues
}

/// Breaks `text` into pieces of at most [`MAX_CUE_CHARS`] characters.
fn chunk(text: &str) -> Vec<String> {
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
    chunks.into_iter().flat_map(hard_split).collect()
}

/// Cuts a chunk the word loop could not break.
///
/// Chinese and Japanese write without spaces between words — voxtype ships
/// sensevoice, paraformer and dolphin for exactly those languages — and a URL
/// has none either, so such text arrives as one oversized chunk and would
/// break the `<= MAX_CUE_CHARS` guarantee this module exists to provide. An
/// arbitrary break at a character boundary is worse typography than a word
/// break and far better than a wall of text in a caption window.
fn hard_split(chunk: String) -> Vec<String> {
    if chunk.chars().count() <= MAX_CUE_CHARS {
        return vec![chunk];
    }
    let mut pieces = Vec::new();
    let mut piece = String::new();
    for c in chunk.chars() {
        piece.push(c);
        if piece.chars().count() == MAX_CUE_CHARS {
            pieces.push(std::mem::take(&mut piece));
        }
    }
    if !piece.is_empty() {
        pieces.push(piece);
    }
    pieces
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
    fn out_of_order_segment_still_yields_end_gt_start() {
        let cues = split(5_000, 1_000, "oops");
        assert_eq!(cues, vec![(5_000, 5_000 + MIN_CUE_MS, "oops".to_string())]);
    }

    /// WebVTT requires `end > start`, so a zero-duration segment gets the
    /// minimum readable cue rather than an instant nobody can display.
    #[test]
    fn a_zero_duration_segment_gets_the_minimum_cue_length() {
        let cues = split(1_000, 1_000, "mm-hm");
        assert_eq!(cues, vec![(1_000, 1_000 + MIN_CUE_MS, "mm-hm".to_string())]);
    }

    /// A long zero-duration segment splits into several cues, and stacking
    /// them all on one instant would show none of them.
    #[test]
    fn zero_duration_cues_are_laid_out_back_to_back() {
        let long = "one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty";
        let cues = split(1_000, 1_000, long);
        assert!(cues.len() > 1);
        for (i, (start, end, _)) in cues.iter().enumerate() {
            assert_eq!(*start, 1_000 + i as u64 * MIN_CUE_MS);
            assert!(end > start, "cue {i}: {start}..{end}");
        }
    }

    #[test]
    fn long_text_splits_and_stays_monotonic() {
        let long = "one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty";
        let cues = split(0, 20_000, long);
        assert!(cues.len() > 1);
        let mut prev_end = 0u64;
        for (start, end, text) in &cues {
            assert_eq!(*start, prev_end);
            assert!(end > start);
            assert!(text.chars().count() <= MAX_CUE_CHARS);
            prev_end = *end;
        }
        assert_eq!(cues.last().unwrap().1, 20_000);
    }

    #[test]
    fn empty_text_yields_no_cues() {
        assert!(split(0, 1000, "").is_empty());
        assert!(split(0, 1000, " \n\t ").is_empty());
    }

    /// `chars * total_duration` overflows `u64` for an `end_ms` a JSON import
    /// can carry, which panicked in a debug build and silently wrapped in a
    /// release one.
    #[test]
    fn an_enormous_end_does_not_overflow() {
        let long = "one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty";
        assert!(long.chars().count() > MAX_CUE_CHARS);
        let cues = split(0, i64::MAX as u64, long);
        assert!(cues.len() > 1);
        let mut prev_end = 0u64;
        for (start, end, _) in &cues {
            assert_eq!(*start, prev_end);
            assert!(end > start);
            prev_end = *end;
        }
        assert_eq!(cues.last().unwrap().1, i64::MAX as u64);
    }

    /// Chinese and Japanese are written without spaces, and voxtype ships
    /// engines for both. The word loop cannot break such text at all, so
    /// without a hard split one cue carries the whole transcript.
    #[test]
    fn text_with_no_whitespace_is_still_split() {
        let cjk: String = "世界各地的人们都在使用这个应用程序来理解会议内容".repeat(11);
        assert!(cjk.chars().count() >= 260, "{}", cjk.chars().count());
        let cues = split(0, 60_000, &cjk);
        assert!(cues.len() >= 4, "got {} cues", cues.len());
        for (_, _, text) in &cues {
            assert!(
                text.chars().count() <= MAX_CUE_CHARS,
                "{} chars in one cue",
                text.chars().count()
            );
        }
        // Nothing is lost or duplicated by the cut.
        let rejoined: String = cues.iter().map(|(_, _, t)| t.as_str()).collect();
        assert_eq!(rejoined, cjk);
    }

    #[test]
    fn a_long_url_is_split_rather_than_left_oversized() {
        let url = format!("https://example.invalid/{}", "a".repeat(200));
        let cues = split(0, 5_000, &url);
        assert!(cues.len() > 1);
        assert!(cues
            .iter()
            .all(|(_, _, t)| t.chars().count() <= MAX_CUE_CHARS));
    }

    /// Defence in depth for the one-line invariant: a blank line inside cue
    /// text terminates the cue and pushes the rest of the segment into the
    /// file as if it were cue syntax.
    #[test]
    fn blank_lines_inside_a_segment_are_collapsed() {
        let cues = split(0, 1_000, "first part\r\n\r\nsecond part");
        assert_eq!(cues, vec![(0, 1_000, "first part second part".to_string())]);
    }
}
