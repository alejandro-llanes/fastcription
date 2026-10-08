//! Selecting words straight out of a transcript line.
//!
//! The gesture the registry is built on: someone in the meeting says a piece
//! of jargon, the reader drags across it — or double-clicks one word — and
//! presses `Ctrl+D` or right-clicks. No form, nothing to type. egui's own
//! label selection draws a selection beautifully and keeps the selected text
//! private, which is no use to anything that wants to *do* something with it;
//! so the transcript owns its selection and paints it itself. The cost is a
//! hundred lines of hit-testing over a galley, which epaint makes cheap: it
//! knows which character is under a point and where each character sits.
//!
//! One selection exists at a time, app-wide, and it remembers everything the
//! registry needs — the words, the line they came from, which conversation,
//! where in it — so the lookup never has to go and find out.

use egui::epaint::text::CharIndex;
use egui::{Color32, CursorIcon, Galley, Id, Pos2, Rect, Response, Sense, Ui, Vec2};
use fc_core::ConversationId;
use std::sync::Arc;

/// Some words of a transcript line, selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// Which row painted it, so only that row draws the highlight and offers
    /// it in its menu.
    pub owner: Id,
    /// Character range into the row's galley text, half-open.
    pub range: (usize, usize),
    /// Where the drag started, so extending it in either direction works.
    anchor: usize,
    /// The selected words.
    pub text: String,
    /// The whole line, for the lookup to explain the words in the light of.
    pub context: String,
    pub conversation: Option<ConversationId>,
    pub start_ms: Option<u64>,
}

/// What a row knows about itself that a selection made in it will need.
pub struct Source<'a> {
    /// The segment's own text, which is what may be selected.
    pub text: &'a str,
    /// Where that text starts in the galley, in characters: rows carry a
    /// speaker and a timestamp in front of it, which are not words anyone
    /// wants to look up.
    pub text_start: usize,
    pub conversation: Option<ConversationId>,
    pub start_ms: Option<u64>,
}

/// Handles the mouse over a painted galley and keeps `selection` current.
///
/// Drag selects a range; a double-click selects the word under the pointer;
/// a plain click clears. The right button is left alone so a context menu on
/// the same response sees the selection it is about to offer.
pub fn interact(
    ui: &mut Ui,
    owner: Id,
    rect: Rect,
    galley: &Arc<Galley>,
    selection: &mut Option<Selection>,
    source: &Source<'_>,
) -> Response {
    let response = ui.interact(rect, owner, Sense::click_and_drag());
    if response.hovered() {
        ui.output_mut(|o| o.cursor_icon = CursorIcon::Text);
    }
    let chars: Vec<char> = galley.job.text.chars().collect();
    let bounds = (
        source.text_start.min(chars.len()),
        (source.text_start + source.text.chars().count()).min(chars.len()),
    );
    // epaint counts in `CharIndex`; everything here counts in `usize`, so the
    // newtype is unwrapped at the boundary and wrapped again for `x_offset`.
    let raw = |pos: Pos2| galley.cursor_from_pos(pos - rect.min).index.0;
    let at = |pos: Pos2| raw(pos).clamp(bounds.0, bounds.1);

    if response.double_clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            // Unclamped on purpose: a double-click on the speaker or the
            // timestamp must select nothing, not the first word of the line.
            let range = word_at(&chars, raw(pos), bounds);
            *selection = make(owner, range, range.0, &chars, source);
        }
    } else if response.drag_started() {
        if let Some(pos) = response.interact_pointer_pos() {
            let index = at(pos);
            *selection = make(owner, (index, index), index, &chars, source);
        }
    } else if response.dragged() {
        if let (Some(pos), Some(current)) = (
            response.interact_pointer_pos(),
            selection.as_ref().filter(|s| s.owner == owner),
        ) {
            let head = at(pos);
            let anchor = current.anchor;
            let range = (anchor.min(head), anchor.max(head));
            *selection = make(owner, range, anchor, &chars, source);
        }
    } else if response.clicked() {
        *selection = None;
    }
    response
}

/// A selection over `range`, or `None` when it holds no word.
fn make(
    owner: Id,
    range: (usize, usize),
    anchor: usize,
    chars: &[char],
    source: &Source<'_>,
) -> Option<Selection> {
    let text: String = chars[range.0..range.1].iter().collect();
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some(Selection {
        owner,
        range,
        anchor,
        text: text.to_owned(),
        context: fc_core::single_line(source.text),
        conversation: source.conversation,
        start_ms: source.start_ms,
    })
}

/// The word around `index`, within `bounds`.
///
/// A word is letters and digits, plus the apostrophes and hyphens inside it:
/// "don't" and "state-of-the-art" are one word each. Punctuation that clings
/// to a spoken word — "ocean," — is not part of it.
pub fn word_at(chars: &[char], index: usize, bounds: (usize, usize)) -> (usize, usize) {
    let (lo, hi) = bounds;
    if lo >= hi || index < lo || index >= hi {
        return (index, index);
    }
    let is_word = |c: char| c.is_alphanumeric() || c == '\'' || c == '\u{2019}' || c == '-';
    if !is_word(chars[index]) {
        return (index, index);
    }
    let mut start = index;
    while start > lo && is_word(chars[start - 1]) {
        start -= 1;
    }
    let mut end = index + 1;
    while end < hi && is_word(chars[end]) {
        end += 1;
    }
    // Apostrophes and hyphens join letters; they do not begin or end a word.
    while start < end && !chars[start].is_alphanumeric() {
        start += 1;
    }
    while end > start && !chars[end - 1].is_alphanumeric() {
        end -= 1;
    }
    (start, end)
}

/// Paints the highlight for `range` behind a galley drawn at `rect.min`.
///
/// One rectangle per wrapped row the range touches, from the x of its first
/// selected character to the x of its last, so a selection that wraps looks
/// like a selection and not like a box.
pub fn paint_highlight(
    ui: &Ui,
    rect: Rect,
    galley: &Galley,
    range: (usize, usize),
    color: Color32,
) {
    let painter = ui.painter();
    let mut row_start = 0;
    for placed in &galley.rows {
        let row_len = placed.char_count_including_newline().0;
        let row_end = row_start + row_len;
        let lo = range.0.max(row_start);
        let hi = range
            .1
            .min(row_start + placed.row.char_count_excluding_newline().0);
        if lo < hi {
            let row_rect = placed.rect().translate(rect.min.to_vec2());
            let x0 = row_rect.left() + placed.row.x_offset(CharIndex(lo - row_start));
            let x1 = row_rect.left() + placed.row.x_offset(CharIndex(hi - row_start));
            painter.rect_filled(
                Rect::from_min_max(
                    Pos2::new(x0, row_rect.top()),
                    Pos2::new(x1, row_rect.bottom()),
                )
                .expand2(Vec2::new(1.0, 1.0)),
                3,
                color,
            );
        }
        row_start = row_end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn a_double_click_takes_the_word_under_the_pointer_without_its_punctuation() {
        let c = chars("boil the ocean, honestly.");
        let b = (0, c.len());
        let word = |i: usize| -> String {
            let (s, e) = word_at(&c, i, b);
            c[s..e].iter().collect()
        };
        assert_eq!(word(0), "boil");
        assert_eq!(word(6), "the");
        assert_eq!(word(12), "ocean");
        assert_eq!(word(14), ""); // the comma
        assert_eq!(word(20), "honestly");
        assert_eq!(word(24), ""); // the full stop
    }

    #[test]
    fn apostrophes_and_hyphens_inside_a_word_are_part_of_it() {
        let c = chars("we don't do state-of-the-art");
        let b = (0, c.len());
        let (s, e) = word_at(&c, 5, b);
        assert_eq!(c[s..e].iter().collect::<String>(), "don't");
        let (s, e) = word_at(&c, 20, b);
        assert_eq!(c[s..e].iter().collect::<String>(), "state-of-the-art");
    }

    /// The speaker and the timestamp in front of the words are not
    /// selectable: a double-click on the timestamp selects nothing.
    #[test]
    fn the_prefix_is_outside_the_bounds() {
        let c = chars("Remote  00:01  ask not");
        let b = (15, c.len());
        let (s, e) = word_at(&c, 3, b);
        assert_eq!(s, e, "a double-click on the prefix selects nothing");
        let (s, e) = word_at(&c, 16, b);
        assert_eq!(c[s..e].iter().collect::<String>(), "ask");
    }

    #[test]
    fn an_empty_range_is_no_selection() {
        let source = Source {
            text: "hello there",
            text_start: 0,
            conversation: None,
            start_ms: Some(3),
        };
        let c = chars("hello there");
        assert!(make(Id::new("r"), (5, 6), 5, &c, &source).is_none()); // the space
        let sel = make(Id::new("r"), (0, 5), 0, &c, &source).unwrap();
        assert_eq!(sel.text, "hello");
        assert_eq!(sel.context, "hello there");
        assert_eq!(sel.start_ms, Some(3));
    }
}
