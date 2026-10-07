//! Reconciles the overlap between two consecutive committed transcripts.
//!
//! Committed chunks share `overlap_ms` of audio (default 0.5 s, see
//! [`crate::segmenter`]) so a word split across a cut boundary is never
//! truncated. That audio gets transcribed twice, so the tail of one committed
//! transcript and the head of the next usually repeat the same words. This is
//! voxtype's own `dedup_bleed_through` problem, one level up.
//!
//! The rule that matters more than the algorithm: **losing real words is
//! worse than leaving a visible duplicate.** A duplicated word in the UI is a
//! minor cosmetic blemish; a dropped word is lost meeting content. Every
//! choice below is biased toward the former.

/// Normalises a word for comparison only: lowercased, with leading/trailing
/// punctuation stripped but internal characters kept (so `"don't"` stays
/// `"don't"`, not `"dont"`). The original word -- casing and punctuation
/// intact -- is what ends up in the returned text; this normalised form never
/// leaves this function.
fn normalize_word(w: &str) -> String {
    w.trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

/// How far back dedup will look for a match. 0.5 s of speech is a handful of
/// words at most; this is a generous ceiling so a long genuine overlap (e.g.
/// backpressure having grown the chunk length) is still caught, while keeping
/// the comparison cheap.
const MAX_OVERLAP_WORDS: usize = 12;

/// Drops the words at the head of `new_text` that duplicate the tail of
/// `prev_text`, returning the remainder of `new_text` with its original
/// casing and punctuation intact.
///
/// Matching is done on normalised (lowercased, punctuation-stripped) words,
/// comparing the longest possible run first: the suffix of `prev_text` and
/// the prefix of `new_text` are compared at every candidate length from
/// `MAX_OVERLAP_WORDS` down to 1, and the *first* (i.e. longest) exact match
/// wins. Checking longest-first is what correctly tells apart an actual
/// overlap duplicate from a legitimate repeated word: for `prev = "...is
/// very"`, `new = "very very good"`, the length-2 candidate (`["is",
/// "very"]` vs `["very", "very"]`) fails, so the length-1 match (`"very"` vs
/// `"very"`) is used instead, which strips exactly the one duplicated word
/// and leaves the legitimate repeat (`"very good"`) alone.
///
/// Returns `new_text` unchanged (words rejoined with single spaces) if no
/// overlap is found, or if either side has no words at all.
pub fn dedup_overlap(prev_text: &str, new_text: &str) -> String {
    let prev_words: Vec<&str> = prev_text.split_whitespace().collect();
    let new_words: Vec<&str> = new_text.split_whitespace().collect();
    if prev_words.is_empty() || new_words.is_empty() {
        return new_text.to_string();
    }

    let prev_norm: Vec<String> = prev_words.iter().map(|w| normalize_word(w)).collect();
    let new_norm: Vec<String> = new_words.iter().map(|w| normalize_word(w)).collect();

    let max_k = MAX_OVERLAP_WORDS.min(prev_norm.len()).min(new_norm.len());
    let mut best_k = 0;
    for k in (1..=max_k).rev() {
        if prev_norm[prev_norm.len() - k..] == new_norm[..k] {
            best_k = k;
            break;
        }
    }

    new_words[best_k..].join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_overlap_leaves_new_text_untouched() {
        let prev = "the meeting starts at noon";
        let new = "we also need the slides ready";
        assert_eq!(dedup_overlap(prev, new), new);
    }

    #[test]
    fn exact_overlap_is_fully_stripped() {
        // The whole of `new` duplicates the tail of `prev`: nothing new was
        // said in this chunk.
        let prev = "we still need owners for the migration work";
        let new = "the migration work";
        assert_eq!(dedup_overlap(prev, new), "");
    }

    #[test]
    fn partial_overlap_strips_only_the_shared_prefix() {
        let prev = "the roadmap review is scheduled for next Tuesday";
        let new = "next Tuesday and we still need owners";
        assert_eq!(dedup_overlap(prev, new), "and we still need owners");
    }

    #[test]
    fn case_and_punctuation_differences_still_match() {
        let prev = "...and we still need owners for the migration work.";
        let new = "Migration work, and the timeline is tight.";
        assert_eq!(dedup_overlap(prev, new), "and the timeline is tight.");
    }

    #[test]
    fn legitimate_word_repetition_is_not_collapsed() {
        // prev ends with a single "very"; new starts with "very very good".
        // Only the first "very" is the real boundary duplicate.
        let prev = "the food here is very";
        let new = "very very good, we should come back";
        assert_eq!(dedup_overlap(prev, new), "very good, we should come back");
    }

    #[test]
    fn longest_match_wins_over_a_shorter_coincidental_one() {
        let prev = "we need this very very";
        let new = "very very good report is due";
        // Length-2 candidate ["very","very"] matches at both ends, so both
        // leading words are stripped, not just one.
        assert_eq!(dedup_overlap(prev, new), "good report is due");
    }

    #[test]
    fn empty_sides_are_left_alone() {
        assert_eq!(dedup_overlap("", "hello there"), "hello there");
        assert_eq!(dedup_overlap("hello there", ""), "");
        assert_eq!(dedup_overlap("", ""), "");
    }
}
