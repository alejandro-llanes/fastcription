//! One transcript row, drawn the same way by the live view and the history
//! view, and the clipboard text that goes with it.
//!
//! A row is **one** `LayoutJob` rather than three labels. Three labels cannot
//! be selected across: a drag that starts on a speaker name and ends two lines
//! down produced nothing, so the only way to get the words out of this app was
//! to export the whole conversation. One galley per row also means the copied
//! text carries its own separators instead of whatever gaps the layout left
//! between widgets.

use std::ops::RangeInclusive;

use egui::text::{LayoutJob, TextFormat};
use egui::{Label, Response, Sense, Stroke};
use fc_core::{Conversation, Segment, Track};

use crate::i18n::t;
use crate::theme::Palette;

/// How large the transcript may be drawn, in points.
///
/// The floor is where Inter stops being comfortable at arm's length; the
/// ceiling is where a sentence at the window's minimum width wraps to three
/// rows and the live view stops showing enough context to follow.
pub const PT_RANGE: RangeInclusive<f32> = 14.0..=40.0;

/// The size `Ctrl+0` returns to, and what a fresh install starts at.
pub const DEFAULT_PT: f32 = 22.0;

/// One press of `Ctrl+=` or `Ctrl+-`. Two points is the smallest step that is
/// visibly a step; one point reads as nothing happening.
pub const PT_STEP: f32 = 2.0;

/// The size to draw at, given whatever is in the settings.
///
/// Not a bare `clamp`: the value is deserialised from a plain-text file in the
/// user's config directory, and `f32::clamp` passes a NaN straight through. A
/// NaN font size reaches epaint and panics there, in a repaint, which costs the
/// meeting that was being transcribed.
pub fn clamp_pt(pt: f32) -> f32 {
    if pt.is_finite() {
        pt.clamp(*PT_RANGE.start(), *PT_RANGE.end())
    } else {
        DEFAULT_PT
    }
}

/// The left margin every row reserves, for the microphone-track bar and the
/// live row's pulse. Kept even on rows that mark nothing, so the text does not
/// shift sideways when a marker appears.
const GUTTER: f32 = 16.0;

/// How long the live row's dot takes to go from dim to bright. Also half its
/// period: the target flips at each end, so the pulse is this long each way.
const PULSE_SECS: f32 = 0.8;

/// Draws one segment and returns the label's response, so the caller can hang
/// a context menu or a `scroll_to_me` off it.
pub fn row(ui: &mut egui::Ui, palette: &Palette, segment: &Segment, pt: f32) -> Response {
    let pt = clamp_pt(pt);
    let (gutter, label) = ui
        .horizontal(|ui| {
            let (gutter, _) = ui.allocate_exact_size(egui::vec2(GUTTER, pt), Sense::hover());
            let label = ui.add(
                Label::new(job(palette, segment, pt))
                    .selectable(true)
                    .wrap(),
            );
            (gutter, label)
        })
        .inner;

    // Painted from the label's own rect rather than the gutter's, so the bar
    // spans a wrapped row's full height instead of one line of it.
    if segment.track == Track::Microphone {
        // Which side of the conversation a line came from is the thing a
        // transcript of a call has to make obvious, and a speaker name repeated
        // down the left edge does not: the eye reads the bar, not the word.
        ui.painter().vline(
            gutter.left() + 2.0,
            label.rect.y_range(),
            Stroke::new(2.0, palette.accent),
        );
    }
    if segment.provisional {
        pulse(
            ui,
            palette,
            egui::pos2(gutter.center().x, label.rect.top() + pt * 0.6),
        );
    }
    ui.add_space(4.0);
    label
}

/// Speaker, time and words in one galley.
fn job(palette: &Palette, segment: &Segment, pt: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.append(
        segment.speaker_label(),
        0.0,
        TextFormat {
            // The microphone track is the user's own voice, so it takes the
            // quieter colour: the side they cannot hear is the one that needs
            // to stand out.
            color: match segment.track {
                Track::Selected => palette.accent,
                Track::Microphone => palette.secondary,
            },
            font_id: fastframe_fonts::Weight::SemiBold.font_id(pt),
            ..Default::default()
        },
    );
    job.append(
        &format!("  {}  ", stamp(segment.start_ms)),
        0.0,
        TextFormat {
            color: palette.secondary,
            font_id: fastframe_fonts::Weight::Regular.font_id(pt * 0.68),
            ..Default::default()
        },
    );
    let translated = segment.translation.is_some();
    job.append(
        &if translated {
            format!("{}\n", segment.text)
        } else {
            segment.text.clone()
        },
        0.0,
        TextFormat {
            // Italics, not a fainter colour, carry "this is not settled yet".
            // The tail of the current pass is the newest and most useful text
            // on screen, and it used to be the least readable thing in the app.
            color: if segment.provisional {
                palette.secondary
            } else {
                palette.text
            },
            italics: segment.provisional,
            font_id: fastframe_fonts::Weight::Regular.font_id(pt),
            ..Default::default()
        },
    );
    // Room for the translation line, whenever something fills it in (D4).
    if let Some(translation) = &segment.translation {
        job.append(
            translation,
            24.0,
            TextFormat {
                color: palette.secondary,
                italics: true,
                font_id: fastframe_fonts::Weight::Regular.font_id(pt * 0.9),
                ..Default::default()
            },
        );
    }
    job
}

/// A dot that breathes, marking the line being spoken right now.
///
/// It replaced the word "provisional" and, before that, nothing at all: a
/// reader following a meeting needs to know at a glance which line is still
/// arriving, and a word in the middle of the transcript is read as transcript.
/// The pulse is driven off the clock rather than a stored flag — the square
/// wave from `time` becomes a triangle through the animator, which also keeps
/// asking for the repaints that animate it.
fn pulse(ui: &mut egui::Ui, palette: &Palette, center: egui::Pos2) {
    let ctx = ui.ctx();
    let half_periods = ctx.input(|i| i.time / f64::from(PULSE_SECS)) as i64;
    let phase = if half_periods % 2 == 0 { 1.0 } else { 0.0 };
    let lit = ctx.animate_value_with_time(ui.id().with("live-pulse"), phase, PULSE_SECS);
    let color = palette.danger.gamma_multiply(0.45 + 0.55 * lit);
    ui.painter().circle_filled(center, 4.0, color);
}

/// `mm:ss`, growing an hour field rather than counting minutes into three
/// digits.
///
/// Zero-padded, unlike [`fc_core::time::duration_hms`], because these run down
/// the left of the transcript as a column and Inter's figures are tabular: a
/// padded minute keeps the words beside them aligned.
fn stamp(start_ms: u64) -> String {
    let total_secs = start_ms / 1000;
    let (hours, minutes, seconds) = (total_secs / 3600, (total_secs % 3600) / 60, total_secs % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

/// One row as text, in the shape `fc_export`'s plain-text writer uses, so a
/// line copied out of the window and a line in an exported file are the same
/// line.
pub fn line_text(segment: &Segment) -> String {
    format!(
        "[{}] {}: {}",
        stamp(segment.start_ms),
        segment.speaker_label(),
        fc_core::single_line(&segment.text)
    )
}

/// Everything on, for a transcript going onto the clipboard: a pasted
/// transcript with no times and no speakers is a wall of sentences, and the
/// user who wanted only the words can select the ones they wanted.
const COPY_OPTIONS: fc_export::ExportOptions = fc_export::ExportOptions {
    timestamps: true,
    speakers: true,
    metadata: true,
};

/// `Copy line` on a right-click, for the one line the pointer is over.
///
/// Hung off the row's own response, which is why [`row`] returns it: a reader
/// following a meeting wants the sentence they just read, not the transcript.
pub fn line_menu(response: &Response, segment: &Segment) -> Option<LineAction> {
    let mut action = None;
    response.context_menu(|ui| {
        if ui.button(t("Add to my words\u{2026}")).clicked() {
            action = Some(LineAction::AddWord);
            ui.close();
        }
        if ui.button(t("Copy line")).clicked() {
            ui.ctx().copy_text(line_text(segment));
            ui.close();
        }
    });
    action
}

/// What a line's menu asked for. Returned rather than done, because the
/// menu is drawn inside a loop that borrows the app's segments and the
/// registry needs the app mutably.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineAction {
    AddWord,
}

/// The Copy and Copy as Markdown buttons a pane header carries.
///
/// Markdown goes through `fc_export`, so what lands on the clipboard is what an
/// exported `.md` file would hold — the same text, without having to choose a
/// destination and find the file afterwards. It needs the conversation record
/// for its heading, so it is offered only once there is one.
pub fn copy_buttons(ui: &mut egui::Ui, conversation: Option<&Conversation>, segments: &[Segment]) {
    let empty = segments.is_empty();
    let record = conversation.filter(|_| !empty);

    let plain_button = |ui: &mut egui::Ui| {
        if ui
            .add_enabled(!empty, egui::Button::new(t("Copy")))
            .on_hover_text(t("The whole transcript as plain text"))
            .clicked()
        {
            ui.ctx().copy_text(plain(segments));
        }
    };
    let markdown_button = |ui: &mut egui::Ui| {
        let button = ui.add_enabled(record.is_some(), egui::Button::new(t("Copy as Markdown")));
        match record {
            Some(conversation) if button.clicked() => {
                ui.ctx().copy_text(fc_export::export(
                    conversation,
                    segments,
                    fc_export::ExportFormat::Markdown,
                    &COPY_OPTIONS,
                ));
            }
            Some(_) => {}
            None => {
                button.on_disabled_hover_text(t(
                    "Markdown carries a heading naming the conversation, and there is no \
                     conversation to name yet.",
                ));
            }
        }
    };

    // Emitted back to front in a right-aligned row, so the pair reads "Copy,
    // Copy as Markdown" on screen either way. A header that aligns its
    // controls to the right places the first widget rightmost, which had the
    // longer, rarer button sitting where the eye looks first.
    if ui.layout().prefer_right_to_left() {
        markdown_button(ui);
        plain_button(ui);
    } else {
        plain_button(ui);
        markdown_button(ui);
    }
}

/// Several rows as text, for the Copy button in a pane header.
pub fn plain<'a>(segments: impl IntoIterator<Item = &'a Segment>) -> String {
    let mut out = String::new();
    for segment in segments {
        if segment.is_blank() {
            continue;
        }
        out.push_str(&line_text(segment));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{clamp_pt, job, line_text, plain, row, stamp, DEFAULT_PT, PT_RANGE};
    use crate::theme::Palette;
    use fastframe_theme::{Base, Palette as ThemePalette};
    use fc_core::{Segment, Track};

    fn palette() -> Palette {
        ThemePalette::base(Base::Dark)
    }

    fn segment(start_ms: u64, text: &str, track: Track) -> Segment {
        Segment {
            track,
            seq: 0,
            start_ms,
            end_ms: start_ms + 1_000,
            text: text.to_owned(),
            translation: None,
            speaker: None,
            confidence: None,
            provisional: false,
        }
    }

    #[test]
    fn timestamps_grow_an_hour_field_instead_of_counting_to_ninety_minutes() {
        assert_eq!(stamp(0), "00:00");
        assert_eq!(stamp(9_000), "00:09");
        assert_eq!(stamp(61_000), "01:01");
        assert_eq!(stamp(3_599_000), "59:59");
        assert_eq!(stamp(3_600_000), "1:00:00");
        assert_eq!(stamp(5_400_000), "1:30:00");
        assert_eq!(stamp(36_000_000), "10:00:00");
    }

    /// The default has to be reachable by `Ctrl+0`, and both sliders have to be
    /// able to show whatever was restored from disk.
    #[test]
    fn the_default_size_is_inside_the_range() {
        assert!(PT_RANGE.contains(&DEFAULT_PT));
    }

    #[test]
    fn a_copied_line_names_its_speaker_and_its_time() {
        let mut mine = segment(61_000, "Sounds good to me.", Track::Microphone);
        mine.speaker = Some("Alice".to_owned());
        assert_eq!(line_text(&mine), "[01:01] Alice: Sounds good to me.");
        assert_eq!(
            line_text(&segment(0, "Let's start.", Track::Selected)),
            "[00:00] Remote: Let's start."
        );
    }

    /// Whisper's silence and music markers are not transcript, and a reader
    /// pasting a transcript into an email should not have to delete them.
    #[test]
    fn copying_skips_what_was_never_said() {
        let segments = vec![
            segment(0, "Hello.", Track::Selected),
            segment(1_000, "[BLANK_AUDIO]", Track::Selected),
            segment(2_000, "Goodbye.", Track::Selected),
        ];
        let copied = plain(&segments);
        assert_eq!(copied, "[00:00] Remote: Hello.\n[00:02] Remote: Goodbye.\n");
    }

    /// A finalised multi-sentence utterance can arrive with newlines in it, and
    /// a row that is one line on screen must be one line on the clipboard.
    #[test]
    fn a_copied_line_is_one_line() {
        let wrapped = segment(0, "First part.\nSecond part.", Track::Selected);
        assert!(!line_text(&wrapped).trim_end().contains('\n'));
    }

    /// The property the whole module exists for: one galley, so a drag can
    /// cross from the speaker into the words and the copy carries both.
    #[test]
    fn a_row_is_one_galley_carrying_speaker_time_and_words() {
        let palette = palette();
        let job = job(
            &palette,
            &segment(61_000, "Hello there.", Track::Selected),
            22.0,
        );
        assert_eq!(job.text, "Remote  01:01  Hello there.");
        assert_eq!(job.sections.len(), 3, "speaker, time, words");
        assert_eq!(job.sections[0].format.color, palette.accent);
        assert_eq!(job.sections[1].format.color, palette.secondary);
        assert_eq!(job.sections[2].format.color, palette.text);
        assert!(!job.sections[2].format.italics);
        // The words are drawn at the chosen size; the time is smaller.
        assert_eq!(job.sections[2].format.font_id.size, 22.0);
        assert!(job.sections[1].format.font_id.size < 22.0);
    }

    /// The newest words on screen. They used to be drawn in `dim`, which on the
    /// light palette was 3:1 against the panel — below AA for the one audience
    /// this app has. Italics carry "not settled yet" instead.
    #[test]
    fn the_unsettled_tail_is_italic_rather_than_faint() {
        let palette = palette();
        let mut pending = segment(0, "and then she", Track::Selected);
        pending.provisional = true;
        let job = job(&palette, &pending, 22.0);
        let words = &job.sections[2].format;
        assert!(words.italics);
        assert_eq!(words.color, palette.secondary);
        assert_ne!(words.color, palette.dim);
        // Same size as settled text: the tail is what the reader is reading.
        assert_eq!(words.font_id.size, 22.0);
    }

    /// The side of the conversation the user cannot hear is the one that has to
    /// stand out, so the microphone track takes the quieter colour.
    #[test]
    fn the_microphone_track_is_the_quieter_of_the_two() {
        let palette = palette();
        let mine = job(&palette, &segment(0, "Right.", Track::Microphone), 22.0);
        assert_eq!(mine.text, "You  00:00  Right.");
        assert_eq!(mine.sections[0].format.color, palette.secondary);
    }

    /// D4's slot: a translation becomes a fourth section on its own line,
    /// inside the same galley, so it is selected and copied with its source.
    #[test]
    fn a_translation_is_a_second_line_of_the_same_galley() {
        let palette = palette();
        let mut translated = segment(0, "Good morning.", Track::Selected);
        translated.translation = Some("Buenos días.".to_owned());
        let job = job(&palette, &translated, 22.0);
        assert_eq!(job.sections.len(), 4);
        assert!(job.text.ends_with("Good morning.\nBuenos días."));
        assert!(job.sections[3].format.italics);
        assert!(job.sections[3].leading_space > 0.0);
    }

    /// The size is deserialised from a text file in the user's config
    /// directory, and `f32::clamp` hands a NaN straight through to epaint,
    /// which panics on it — in a repaint, during a meeting.
    #[test]
    fn an_impossible_size_falls_back_instead_of_reaching_the_font_stack() {
        assert_eq!(clamp_pt(f32::NAN), DEFAULT_PT);
        assert_eq!(clamp_pt(f32::INFINITY), DEFAULT_PT);
        assert_eq!(clamp_pt(0.0), *PT_RANGE.start());
        assert_eq!(clamp_pt(1000.0), *PT_RANGE.end());
        assert_eq!(clamp_pt(DEFAULT_PT), DEFAULT_PT);
    }

    /// Lays a row out for real, through egui, at both ends of the range and at
    /// a size that should never have got this far.
    ///
    /// No window and no GPU: `Context::run` is the whole frame. It is worth the
    /// 200 ms because the row asks for a font family by name — Inter at
    /// semibold — and epaint panics outright on a family nothing registered.
    #[test]
    fn a_row_lays_out_through_egui_without_panicking() {
        let ctx = egui::Context::default();
        ctx.set_fonts(
            fastframe_fonts::FontSetup::default()
                .system_fallbacks(false)
                .definitions(),
        );
        let palette = palette();
        let mut pending = segment(5_000, "still arriving", Track::Microphone);
        pending.provisional = true;

        let mut output = ctx.run_ui(Default::default(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                for pt in [*PT_RANGE.start(), DEFAULT_PT, *PT_RANGE.end(), f32::NAN] {
                    let settled = row(ui, &palette, &segment(0, "Hello.", Track::Selected), pt);
                    assert!(settled.rect.height() > 0.0, "nothing laid out at {pt}");
                    // The provisional row paints a pulse and the microphone row
                    // a bar, both off the label's own rect.
                    let live = row(ui, &palette, &pending, pt);
                    assert!(live.rect.height() > 0.0);
                }
            });
        });
        // The frame really rasterised glyphs, and epaint refuses to let a
        // texture upload be dropped on the floor.
        output.textures_delta.clear();
    }
}
