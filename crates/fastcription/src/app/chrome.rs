//! The window itself: what is re-applied to every fresh `egui::Context`, the
//! frame that lays the panels out, the top bar, the notices, compact mode, the
//! tray and the theme.
//!
//! Split out of `app/mod.rs` so the chrome — the part that has to run again on
//! every reopened window — can be read without the pipeline and the library in
//! the way.

use std::time::Instant;

use egui::{Key, KeyboardShortcut, Modifiers, RichText, ViewportCommand, WindowLevel};
use fc_core::{AudioSource, Pressure, SessionState};

use crate::app::{
    session_control::format_elapsed, transcript, App, MainView, NoticeKind, ServiceStatus,
};
use crate::i18n::{t, tf};

/// The window's ordinary title, and the one it takes in compact mode.
///
/// The compact title is what a compositor rule matches on (`docs/OVERLAY.md`),
/// so it is a contract with the user's configuration, not decoration.
const TITLE: &str = "fastcription";
pub(super) const COMPACT_TITLE: &str = "fastcription — captions";

/// Compact mode's window size: wide enough for a sentence at 22 pt, short
/// enough to sit under a video call without covering a face.
const COMPACT_SIZE: [f32; 2] = [760.0, 170.0];
/// The main window's size minimum, matching what `main.rs` opens it with.
const MAIN_MIN_SIZE: [f32; 2] = [760.0, 480.0];

/// The source picker's width. Wide enough for a sink input's application name
/// and most device descriptions, narrow enough that the transport still fits
/// beside it at the window's 760 pt minimum.
/// How tall the readouts' half of the footer is.
const STATUS_BAR_HEIGHT: f32 = 52.0;

/// How far the spectrum reaches above that bar.
///
/// The bar is translucent and the spectrum is drawn behind it, so the loud
/// bands break out of the top rather than being clipped flat by it. The first
/// version gave this 22 points, which let the loudest band poke a few points
/// over the edge and no more; the reference it was drawn from has the bars
/// standing well clear of the bar, and that takes room.
const STATUS_SPECTRUM_RISE: f32 = 64.0;

/// How opaque the footer is over the spectrum behind it.
///
/// Nothing on the bar is read against this. Every label sits on an opaque
/// plate and every state word on a chip, which is what lets the scrim be
/// light enough for the spectrum to actually show — the first attempt drew
/// the labels straight onto it, and the arithmetic said the bar had to be 96%
/// opaque to keep them readable, which is the same as not drawing a spectrum
/// at all. The scrim's remaining job is to stop the bands being so loud
/// behind the plates that the bar stops reading as a bar.
const SCRIM: f32 = 0.45;

/// The fallback level meter's size, for when the spectrum is turned off.
const STATUS_VISUALIZER_WIDTH: f32 = 140.0;
const STATUS_VISUALIZER_HEIGHT: f32 = 16.0;

/// The compact strip's buttons, square and round-cornered into circles.
///
/// `COMPACT_VISUALIZER_RANGE`'s floor is set from this: the row is as tall as
/// the spectrum, and the buttons sit in the middle of it.
const COMPACT_BUTTON: f32 = 26.0;

/// The compact strip's centre button, the one the hand goes to.
const COMPACT_BIG_BUTTON: f32 = 38.0;

const COMBO_WIDTH: f32 = 230.0;
/// How many characters of a source name the picker shows. Chosen to sit inside
/// `COMBO_WIDTH` at the default text size.
const COMBO_CHARS: usize = 34;

/// How much of the source's name the footer readout spells out. Shorter than
/// the picker's: this one is a reminder of a choice already made, not the
/// control that makes it.
const SOURCE_READOUT_CHARS: usize = 28;

impl App {
    /// Re-applies everything tied to an `egui::Context`: fonts, text
    /// rendering, icons, and the current palette. Called once per window —
    /// including every reopen from the tray, since each gets its own context.
    pub fn attach(&mut self, ctx: &egui::Context) {
        let mut fonts = fastframe_fonts::FontSetup::default().definitions();
        self.text_rendering.apply_to(&mut fonts);
        ctx.set_fonts(fonts);
        ctx.all_styles_mut(|style| self.text_rendering.apply_to_visuals(&mut style.visuals));

        egui_extras::install_image_loaders(ctx);
        fastframe_icons::install::<crate::icons::Icon>(ctx);

        self.theme_waker = fastframe_theme::Waker::new({
            let ctx = ctx.clone();
            move || ctx.request_repaint()
        });
        self.theme_catalog
            .start(self.themes_dir.clone(), None, &self.theme_waker);

        self.palette.apply(ctx);

        // A fresh viewport is titled from `native_options`, whatever the last
        // one was told, so what we believe the compositor knows is wrong until
        // `sync_title` has spoken to this one.
        self.title_shown.clear();

        // A window reopened from the tray while compact was on comes back
        // decorated and ordinary-sized, because the commands that made it
        // compact were sent to a viewport that no longer exists.
        if self.compact {
            self.apply_compact(ctx);
        }
    }

    /// The whole window's content for one pass. `ui` is the viewport's root
    /// `Ui` (see this eframe fork's `App::ui`, not the usual `update`).
    pub fn frame(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.scrolling.apply(&ctx);
        self.drain_events(&ctx);
        self.poll_startup();
        self.poll_remote_probe();
        self.poll_import();
        if let Some(status) = self.service_monitor.poll() {
            self.voxtype_service = status;
        }
        self.update_search(&ctx);
        self.poll_theme(&ctx);
        self.drain_tray(&ctx, false);
        // A second launch asked for this window rather than starting another
        // copy of the app.
        if self.take_show_request() {
            ctx.send_viewport_cmd(ViewportCommand::Focus);
        }
        if self.quit_requested {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
        if let Some(due) = self.notices.expire(Instant::now()) {
            ctx.request_repaint_after(due);
        }
        self.handle_shortcuts(&ctx);
        self.sync_title(&ctx);

        if self.compact {
            self.compact_frame(ui);
            return;
        }

        self.delete_confirmation(&ctx);

        egui::Panel::top("top-bar")
            .frame(crate::ui::bar(&self.palette, true))
            .show(ui, |ui| self.top_bar(ui));
        self.notice_panel(ui);
        // Below the notices and above the sidebar, so it spans the window:
        // what it carries is about the session, not about either pane.
        // Tall enough for the readouts, plus a strip above them the spectrum
        // can rise into. The panel reserves both, so the transcript stops
        // above the bars rather than scrolling behind them — the one thing
        // this layout must not borrow from a music player, where whatever
        // slides under the bar was never going to be read.
        egui::Panel::bottom("status")
            .exact_size(STATUS_BAR_HEIGHT + STATUS_SPECTRUM_RISE)
            .frame(egui::Frame::NONE)
            .show(ui, |ui| self.status_bar(ui));

        egui::Panel::left("sidebar")
            .resizable(true)
            .default_size(260.0)
            .size_range(200.0..=420.0)
            .frame(
                egui::Frame::default()
                    .fill(self.palette.panel)
                    .inner_margin(egui::Margin::symmetric(10, 10)),
            )
            .show(ui, |ui| super::sidebar::show(self, ui));

        // The ground the panels sit on, and the frame the transcript is read
        // out of.
        egui::CentralPanel::default()
            .frame(
                egui::Frame::default()
                    .fill(self.palette.window)
                    .inner_margin(egui::Margin::same(12)),
            )
            .show(ui, |ui| match self.main_view {
                MainView::Live => super::live::show(self, ui),
                MainView::History(id) => super::history::show(self, ui, id),
                MainView::Settings => super::settings::show(self, ui),
            });
    }

    /// Every key the window answers to.
    ///
    /// Nothing fires while a text field has the keyboard: the app is full of
    /// them — a conversation title, a server address, a new tag — and `Ctrl+R`
    /// stopping being "select the word" and starting a recording mid-sentence
    /// is the kind of surprise that costs a meeting. `text_edit_focused` rather
    /// than `egui_wants_keyboard_input`, which is true of any focused widget
    /// and would silence the transport for as long as a button held focus.
    ///
    /// `Esc` only leaves compact mode while the window has focus, which is the
    /// point: a compositor rule that keeps the captions above a call usually
    /// also keeps focus in the call, and then nothing here sees the key at all
    /// — hence the restore button compact mode draws for itself.
    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        // Trace-level only: what a key press looked like by the time it reached
        // egui is the one thing a shortcut that "does nothing" needs recorded,
        // and it is far too chatty for any other level.
        if tracing::enabled!(tracing::Level::TRACE) {
            ctx.input(|i| {
                for event in &i.events {
                    if let egui::Event::Key {
                        key,
                        pressed,
                        modifiers,
                        ..
                    } = event
                    {
                        tracing::trace!(?key, pressed, ?modifiers, "key event");
                    }
                }
            });
        }
        if ctx.text_edit_focused() {
            return;
        }

        let shortcut = |modifiers: Modifiers, key: Key| {
            ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(modifiers, key)))
        };

        // Not Ctrl+M: on Linux egui's winit layer turns the press of
        // any Ctrl+C combination into a Copy event and delivers only the
        // release as a key, so that shortcut could never match. Verified by
        // tracing the events the window received.
        if shortcut(Modifiers::CTRL, Key::M) {
            self.set_compact(ctx, !self.compact);
        }
        // No focus check: Wayland only delivers a key to the focused surface,
        // so a key event already proves focus, and the extra condition only
        // ever lost an Escape.
        if self.compact && ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.set_compact(ctx, false);
        }

        // Settings was reachable only by its button in the top bar, which is
        // the one pane with no other way in. Ctrl+, is what the rest of the
        // desktop uses. It toggles, so the same key puts the transcript back
        // rather than stranding the reader in a settings pane.
        if shortcut(Modifiers::CTRL, Key::Comma) {
            self.main_view = if self.main_view == MainView::Settings {
                MainView::Live
            } else {
                MainView::Settings
            };
        }

        for (modifiers, key, request) in [
            (Modifiers::CTRL, Key::R, TransportKey::StartOrResume),
            (Modifiers::CTRL, Key::Space, TransportKey::PauseOrResume),
            (Modifiers::CTRL, Key::Period, TransportKey::Stop),
        ] {
            if shortcut(modifiers, key) {
                self.apply_transport(transport_for(request, self.state));
            }
        }

        // `Plus` as well as `Equals`, because the key is shifted on most
        // layouts and winit reports what the shift produced.
        let pt = self.settings.transcript_pt;
        if shortcut(Modifiers::CTRL, Key::Equals) || shortcut(Modifiers::CTRL, Key::Plus) {
            self.set_transcript_pt(pt + transcript::PT_STEP);
        }
        if shortcut(Modifiers::CTRL, Key::Minus) {
            self.set_transcript_pt(pt - transcript::PT_STEP);
        }
        if shortcut(Modifiers::CTRL, Key::Num0) {
            self.set_transcript_pt(transcript::DEFAULT_PT);
        }
    }

    /// Clamps the transcript size to the one range the sliders and the
    /// shortcuts share, so a size reached by keyboard is a size a slider can
    /// show.
    pub(super) fn set_transcript_pt(&mut self, pt: f32) {
        self.settings.transcript_pt = transcript::clamp_pt(pt);
    }

    fn apply_transport(&mut self, action: Option<Transport>) {
        match action {
            Some(Transport::Start) | Some(Transport::Resume) => self.start_or_resume(),
            Some(Transport::Pause) => self.pause(),
            Some(Transport::Stop) => self.stop(),
            None => {}
        }
    }

    /// Keeps the window title saying what the session is doing, which on a
    /// tiled desktop is the only part of fastcription visible while the user is
    /// looking at the call.
    ///
    /// Compact mode owns the title: [`COMPACT_TITLE`] is what the compositor
    /// rule in `docs/OVERLAY.md` matches on, so a `REC` prefix there would
    /// float the captions for exactly as long as it took the clock to tick.
    fn sync_title(&mut self, ctx: &egui::Context) {
        if self.compact {
            return;
        }
        let wanted = match self.state {
            // Truncated to the second by `duration_hms`, so this string — and
            // therefore the command below — changes once a second, not once
            // per frame.
            SessionState::Recording => format!(
                "\u{25cf} REC {} \u{2014} {TITLE}",
                fc_core::time::duration_hms(self.elapsed().as_millis() as u64)
            ),
            SessionState::Paused => format!("\u{23f8} Paused \u{2014} {TITLE}"),
            SessionState::Finishing => format!("Finishing \u{2014} {TITLE}"),
            SessionState::Idle => TITLE.to_owned(),
        };
        if wanted != self.title_shown {
            ctx.send_viewport_cmd(ViewportCommand::Title(wanted.clone()));
            self.title_shown = wanted;
        }
    }

    /// Enters or leaves compact mode.
    ///
    /// Decision D3 called for a second, always-on-top window. It was built as
    /// an egui *deferred* viewport and never worked: the app's repaint requests
    /// target the root viewport, so the captions froze after their first frame,
    /// and hiding the main window to the tray destroyed the child along with
    /// its parent. One window that changes shape has none of those problems —
    /// the cost is that hiding to the tray hides the captions too, which is
    /// written down in `docs/OVERLAY.md`.
    pub(super) fn set_compact(&mut self, ctx: &egui::Context, compact: bool) {
        if compact == self.compact {
            return;
        }
        self.compact = compact;
        // Compact mode may wear a different palette, and `poll_theme` only
        // re-resolves when something it watches changed. Forgetting what was
        // applied is what makes the next frame notice.
        self.theme_applied = None;
        if compact {
            self.apply_compact(ctx);
        } else {
            // The size to go back to is the compositor's to remember, since it
            // is the compositor that changed it; the minimum is this window's
            // own and has to be put back, or nothing stops the user dragging
            // the restored window down to a caption bar's height.
            ctx.send_viewport_cmd(ViewportCommand::MinInnerSize(MAIN_MIN_SIZE.into()));
            crate::compositor::leave_compact();
            ctx.send_viewport_cmd(ViewportCommand::Decorations(true));
            ctx.send_viewport_cmd(ViewportCommand::WindowLevel(WindowLevel::Normal));
            // Not sent here: `sync_title` owns the ordinary title and knows
            // whether the session is recording, which this does not. Clearing
            // what the compositor was last told is what makes it send one.
            self.title_shown.clear();
        }
    }

    fn apply_compact(&mut self, ctx: &egui::Context) {
        // Both halves are needed. The size minimum is the window's own, and a
        // compositor honours it, so without lowering it first the caption bar
        // is clamped to `MAIN_MIN_SIZE` — measured: a resize to 760x170 landed
        // at 760x480. Lowering the minimum works (it is a different winit
        // call); only the resize itself has to go through the compositor.
        ctx.send_viewport_cmd(ViewportCommand::MinInnerSize(COMPACT_SIZE.into()));
        crate::compositor::enter_compact(COMPACT_SIZE);
        ctx.send_viewport_cmd(ViewportCommand::Decorations(false));
        ctx.send_viewport_cmd(ViewportCommand::WindowLevel(WindowLevel::AlwaysOnTop));
        ctx.send_viewport_cmd(ViewportCommand::Title(COMPACT_TITLE.to_owned()));
        self.title_shown = COMPACT_TITLE.to_owned();
    }

    /// Start, pause or resume, as one big button in the middle of the strip.
    ///
    /// Compact mode is a strip on top of a call, and reaching the transport
    /// meant restoring the whole window, pressing a button and shrinking
    /// again — three actions and a window resize to stop transcribing a
    /// coffee break. Returns whether it was pressed rather than acting, so the
    /// caller can act outside the closure that borrows `self` for drawing.
    ///
    /// Pause rather than stop, because this is a toggle and stop is not
    /// reversible: pressing it twice has to leave one conversation with a gap
    /// in it, not two conversations. `Ctrl+.` still stops, from here as well
    /// as from the full window.
    fn compact_toggle_button(&mut self, ui: &mut egui::Ui, at: egui::Rect) -> bool {
        use crate::icons::Icon;

        let (icon, hover) = match self.state {
            SessionState::Recording => (Icon::Pause, t("Pause transcribing (Ctrl+Space)")),
            SessionState::Paused => (Icon::Play, t("Carry on transcribing (Ctrl+Space)")),
            SessionState::Finishing => (Icon::Pause, t("Finishing the last of the audio")),
            SessionState::Idle => (Icon::Play, t("Start transcribing (Ctrl+R)")),
        };
        // Lit whenever pressing it would do something, the same rule as the
        // main bar's; the icon is what says whether anything is being
        // transcribed, and the state light in the full window says it too.
        let enabled = self.state != SessionState::Finishing && !self.library_read_only;
        let active = enabled;
        let palette = self.palette.clone();
        let response = ui
            .scope_builder(egui::UiBuilder::new().max_rect(at), |ui| {
                if !enabled {
                    ui.disable();
                }
                crate::ui::big_button_at(ui, &palette, icon, active, at)
            })
            .inner;
        if self.library_read_only {
            response.on_disabled_hover_text(crate::env::read_only_library(&self.library_path));
            return false;
        }
        response.on_hover_text(hover).clicked()
    }

    /// The compact strip's visualiser height, clamped to what the strip can
    /// actually give it.
    ///
    /// A value restored from disk is not trusted: the settings file is plain
    /// text a user can edit, and a height taller than the strip would leave no
    /// captions at all — which is the one thing compact mode exists to show.
    pub(super) fn compact_visualizer_height(&self) -> f32 {
        super::settings::clamp_visualizer_height(self.settings.compact_visualizer_height)
    }

    /// What the compact button and `Ctrl+Space` both mean: transcribing, or
    /// not.
    fn toggle_transcribing(&mut self) {
        match self.state {
            SessionState::Idle | SessionState::Paused => self.start_or_resume(),
            SessionState::Recording => self.pause(),
            // Already on its way to idle, and starting again here would race
            // the transcription of the audio still in flight.
            SessionState::Finishing => {}
        }
    }

    /// One caption line, laid out exactly as it will be painted.
    ///
    /// Both the fitting and the drawing go through here, which is the whole
    /// point: a line measured in one face and drawn in another wraps to a
    /// different number of rows, and the difference lands on the reader as
    /// text over the controls.
    fn caption_galley(
        &self,
        ui: &egui::Ui,
        text: &str,
        unsettled: bool,
        size: f32,
        wrap: f32,
    ) -> std::sync::Arc<egui::Galley> {
        let mut job = egui::text::LayoutJob::default();
        job.wrap.max_width = wrap;
        job.append(
            text,
            0.0,
            egui::TextFormat {
                font_id: egui::FontId::proportional(size),
                // The in-flight tail is replaced on every pass, so it is marked
                // as not yet settled — in italics, at a colour that still
                // clears AA. It used to be drawn in `dim`, which made the
                // newest words on screen the hardest ones to read.
                color: if unsettled {
                    self.palette.secondary
                } else {
                    self.palette.text
                },
                italics: unsettled,
                ..Default::default()
            },
        );
        ui.painter().layout_job(job)
    }

    /// Captions and nothing else: the last few committed lines, then the one
    /// being spoken, in italics because its tail is still being revised.
    fn compact_frame(&mut self, ui: &mut egui::Ui) {
        let size = transcript::clamp_pt(self.settings.transcript_pt);
        let mut leave = false;
        egui::CentralPanel::default()
            .frame(
                egui::Frame::default()
                    .fill(self.palette.window)
                    .inner_margin(12.0),
            )
            .show(ui, |ui| {
                let mut toggle = false;
                // The controls float over the spectrum rather than sitting
                // beside it: the strip is 760 points of which the captions
                // want every one, and a row that spends a third of its width
                // on a clock and two buttons leaves the spectrum a slot
                // instead of a span. Transparent frame and an exact height,
                // because everything in here is placed by hand.
                let row = self.compact_visualizer_height();
                egui::Panel::bottom("compact-controls")
                    .exact_size(row)
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| {
                        let rect = ui.max_rect();
                        self.visualizer
                            .paint_at(ui, rect, &self.palette, self.settings.visualizer);
                        // The same arrangement as the main window's footer:
                        // a light scrim so the strip still reads as a strip,
                        // and anything carrying words on an opaque chip of
                        // its own rather than straight onto the spectrum.
                        ui.painter().rect_filled(
                            rect,
                            0,
                            self.palette.window.gamma_multiply(SCRIM),
                        );

                        let middle = egui::Rect::from_center_size(
                            rect.center(),
                            egui::vec2(COMPACT_BIG_BUTTON, COMPACT_BIG_BUTTON),
                        );
                        toggle = self.compact_toggle_button(ui, middle);

                        // Stop, small, to the left of the toggle: the one
                        // transport action the toggle cannot be, and until
                        // now reachable from compact mode only by shortcut.
                        let stoppable =
                            matches!(self.state, SessionState::Recording | SessionState::Paused);
                        let stop_rect = egui::Rect::from_center_size(
                            egui::Pos2::new(
                                middle.left() - 10.0 - COMPACT_BUTTON / 2.0,
                                rect.center().y,
                            ),
                            egui::vec2(COMPACT_BUTTON, COMPACT_BUTTON),
                        );
                        let stop = ui
                            .scope_builder(egui::UiBuilder::new().max_rect(stop_rect), |ui| {
                                if !stoppable {
                                    ui.disable();
                                }
                                ui.put(
                                    stop_rect,
                                    egui::Button::image(
                                        crate::icons::Icon::Stop.image(self.palette.text, 14.0),
                                    )
                                    .fill(self.palette.surface)
                                    .corner_radius(COMPACT_BUTTON / 2.0),
                                )
                            })
                            .inner;
                        if stop.on_hover_text(t("Stop (Ctrl+.)")).clicked() {
                            self.stop();
                        }

                        // Left of the clock and right of Restore, the row is
                        // nothing but spectrum, which is the point.
                        let clock = egui::Rect::from_min_max(
                            egui::Pos2::new(rect.left(), rect.top()),
                            egui::Pos2::new(middle.left() - 20.0 - COMPACT_BUTTON, rect.bottom()),
                        );
                        ui.scope_builder(egui::UiBuilder::new().max_rect(clock), |ui| {
                            ui.with_layout(
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    crate::ui::plate(ui, &self.palette, |ui| {
                                        ui.label(
                                            RichText::new(format_elapsed(self.elapsed()))
                                                .small()
                                                .monospace()
                                                .color(self.palette.secondary),
                                        );
                                    });
                                },
                            );
                        });

                        let restore = egui::Rect::from_center_size(
                            egui::Pos2::new(rect.right() - COMPACT_BUTTON / 2.0, rect.center().y),
                            egui::vec2(COMPACT_BUTTON, COMPACT_BUTTON),
                        );
                        leave = ui
                            .put(
                                restore,
                                egui::Button::image(
                                    crate::icons::Icon::Restore.image(self.palette.text, 14.0),
                                )
                                .fill(self.palette.surface)
                                .corner_radius(COMPACT_BUTTON / 2.0),
                            )
                            .on_hover_text(t("Back to the full window (Esc)"))
                            .clicked();
                    });
                if toggle {
                    self.toggle_transcribing();
                }

                // Newest at the bottom, whole lines only, and clipped to the
                // room that is actually left.
                //
                // This used to measure every line in the regular face and then
                // draw the line being spoken in italics. Italic glyphs are not
                // the same width, so a provisional line could wrap to one more
                // row than it had been measured at — and since the newest
                // lines are the provisional ones, a strip full of them walked
                // straight down over the controls. Laying out once and
                // painting *that* galley is the only arrangement where the two
                // cannot drift apart; clipping the painter means even a line
                // longer than the whole strip stays inside it.
                let (area, _) = ui.allocate_exact_size(ui.available_size(), egui::Sense::hover());
                let painter = ui.painter_at(area);
                let spacing = ui.spacing().item_spacing.y;

                let mut lines: Vec<(&str, bool)> = Vec::new();
                let mut pending: Vec<&fc_core::Segment> = self.provisional.values().collect();
                pending.sort_by_key(|segment| segment.seq);
                // Newest first: this paints upward from the bottom edge.
                for segment in pending.iter().rev() {
                    lines.push((segment.text.as_str(), true));
                }
                for segment in self.segments.iter().rev() {
                    lines.push((segment.text.as_str(), false));
                }

                if lines.is_empty() {
                    let galley =
                        self.caption_galley(ui, t("Waiting for speech…"), true, size, area.width());
                    painter.galley(area.left_top(), galley, self.palette.secondary);
                } else {
                    let mut baseline = area.bottom();
                    for (index, (text, unsettled)) in lines.iter().enumerate() {
                        let galley = self.caption_galley(ui, text, *unsettled, size, area.width());
                        let top = baseline - galley.size().y;
                        // A line cut horizontally through its glyphs reads as
                        // broken rather than as scrolled, so a line that does
                        // not fit whole is left out. The newest line is the
                        // exception: showing the last rows of what is being
                        // said beats showing nothing at all.
                        if top < area.top() && index > 0 {
                            break;
                        }
                        // That exception still may not slice a row in half. An
                        // utterance can run longer than the whole strip — a
                        // speaker who does not pause builds one segment for as
                        // long as it takes — so the galley is pushed down until
                        // the first row that still fits starts exactly at the
                        // top edge, and the clip falls between rows.
                        let top = if top < area.top() {
                            top + first_visible_row(&galley, area.top() - top)
                        } else {
                            top
                        };
                        painter.galley(
                            egui::Pos2::new(area.left(), top),
                            galley,
                            self.palette.text,
                        );
                        baseline = top - spacing;
                        if baseline <= area.top() {
                            break;
                        }
                    }
                }
            });
        if leave {
            let ctx = ui.ctx().clone();
            self.set_compact(&ctx, false);
        }
    }

    /// Everything that chooses or starts a conversation. Status moved out of
    /// here into [`App::status_bar`]: the bar was one non-wrapping `horizontal`
    /// carrying a level meter, a clock, a pressure warning and a service pill
    /// as well, and below about a thousand points — the window's minimum is
    /// 760 — the right-hand controls were simply off the edge.
    fn top_bar(&mut self, ui: &mut egui::Ui) {
        use crate::ui;

        let palette = self.palette.clone();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;

            ui::wordmark(ui, &palette);
            ui.add_space(10.0);

            // What the session is doing, in colour, because it is the one
            // thing on this bar that changes by itself.
            let (state, colour) = match self.state {
                SessionState::Recording => (t("REC"), palette.accent),
                SessionState::Paused => (t("PAUSED"), palette.warning),
                SessionState::Finishing => (t("FINISHING"), palette.warning),
                SessionState::Idle => (t("IDLE"), palette.dim),
            };
            ui.add_space(2.0);
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.add(dot(colour, self.state == SessionState::Recording));
                ui.label(
                    RichText::new(state)
                        .size(ui::LABEL_PT + 1.0)
                        .color(colour)
                        .strong(),
                );
            });

            ui.add_space(4.0);
            source_combo(self, ui, "top-bar-source");

            ui.add_space(4.0);
            self.transport(ui);

            ui.add_space(4.0);
            mic_checkbox(self, ui, t("Mic"));

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if ui::icon_button(ui, &palette, crate::icons::Icon::Compact, self.compact)
                    .on_hover_text(t("Captions only, always on top (Ctrl+M). Esc comes back."))
                    .clicked()
                {
                    let ctx = ui.ctx().clone();
                    self.set_compact(&ctx, true);
                }
                // Right-to-left, so this is added after the button it sits to
                // the left of.
                let chosen = usize::from(self.main_view == MainView::Settings);
                if let Some(index) =
                    ui::segmented(ui, &palette, &[t("Live"), t("Settings")], chosen)
                {
                    self.main_view = if index == 0 {
                        MainView::Live
                    } else {
                        MainView::Settings
                    };
                }
            });
        });
    }

    /// Start, Pause and Stop, with the icons and the shortcuts they answer to.
    ///
    /// The shortcut is in every tooltip rather than in the label: the labels
    /// have to stay short enough that three of them plus a source picker fit
    /// across the narrowest window the app allows.
    fn transport(&mut self, ui: &mut egui::Ui) {
        use crate::icons::Icon;
        use crate::ui;

        let palette = self.palette.clone();
        // One big button that starts, pauses and resumes, like a player's: the
        // icon says which it will do, and it is lit whenever pressing it would
        // do something. The first version made it Start alone, which left the
        // largest control on the bar sitting there disabled for the whole of
        // every recording — exactly when the hand goes looking for it.
        let (icon, hover) = match self.state {
            SessionState::Recording => (Icon::Pause, t("Pause (Ctrl+Space)")),
            SessionState::Paused => (Icon::Play, t("Resume (Ctrl+R)")),
            SessionState::Finishing => (Icon::Pause, t("Finishing the last of the audio")),
            SessionState::Idle => (Icon::Play, t("Start (Ctrl+R)")),
        };
        let enabled = self.state != SessionState::Finishing && !self.library_read_only;
        let toggle = ui
            .scope(|ui| {
                if !enabled {
                    ui.disable();
                }
                ui::big_button(ui, &palette, icon, enabled, ui::BIG_BUTTON)
            })
            .inner;
        if self.library_read_only {
            toggle.on_disabled_hover_text(crate::env::read_only_library(&self.library_path));
        } else if toggle.on_hover_text(hover).clicked() {
            self.toggle_transcribing();
        }

        let stoppable = matches!(self.state, SessionState::Recording | SessionState::Paused);
        if ui
            .scope(|ui| {
                if !stoppable {
                    ui.disable();
                }
                ui::icon_button(ui, &palette, Icon::Stop, false)
            })
            .inner
            .on_hover_text(t("Stop (Ctrl+.)"))
            .clicked()
        {
            self.stop();
        }
    }

    /// What the session is doing, on a line of its own at the foot of the
    /// window: the input level, how long it has been running, whether
    /// transcription is keeping up, and whether voxtype's daemon is there.
    ///
    /// None of it is a control, which is why it is down here and not next to
    /// the buttons.
    fn status_bar(&mut self, ui: &mut egui::Ui) {
        use crate::ui;

        let palette = self.palette.clone();
        let strip = ui.max_rect();
        let bar = egui::Rect::from_min_max(
            egui::Pos2::new(strip.left(), strip.bottom() - STATUS_BAR_HEIGHT),
            strip.max,
        );

        // Painted in three passes, because the spectrum goes *behind* the bar
        // rather than beside it: the ground first, then the spectrum across
        // the whole strip, then a translucent bar over the lower part of it.
        // The bars that reach above the bar are left uncovered, which is what
        // makes the thing look like it is coming up out of the floor instead
        // of sitting in a box.
        let painter = ui.painter();
        painter.rect_filled(strip, 0, palette.window);
        let spectrum = self.settings.visualizer_in_main;
        if spectrum {
            self.visualizer
                .paint_at(ui, strip, &palette, self.settings.visualizer);
        }
        let painter = ui.painter();
        painter.rect_filled(bar, 0, palette.panel.gamma_multiply(SCRIM));
        painter.hline(
            bar.x_range(),
            bar.top(),
            egui::Stroke::new(1.0, palette.outline),
        );

        let content = bar.shrink2(egui::vec2(12.0, 8.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(content), |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 14.0;

                // The meter reads the *selected* source, which is usually the
                // other end of a call. A microphone icon here said the
                // opposite, and a user watching a flat meter while the remote
                // side talked had every reason to think they had picked the
                // wrong thing.
                let source = self
                    .selected_source
                    .and_then(|index| self.sources.get(index))
                    .map(|source| source.label());
                let hover = match &source {
                    Some(label) => tf("Input level from {}", &[label]),
                    None => t("No audio source is selected").to_owned(),
                };

                ui::plate(ui, &palette, |ui| {
                    ui.horizontal_centered(|ui| {
                        ui.spacing_mut().item_spacing.x = 14.0;
                        ui::readout(
                            ui,
                            &palette,
                            t("elapsed"),
                            &format_elapsed(self.elapsed()),
                            self.state == SessionState::Recording,
                        );
                        if let Some(label) = source {
                            separator(ui, &palette);
                            ui::readout(
                                ui,
                                &palette,
                                t("source"),
                                &elide(&label, SOURCE_READOUT_CHARS),
                                false,
                            );
                        }
                    });
                });

                // Only when the spectrum is not drawn: with it behind the bar
                // there is already a picture of the input, and a percentage
                // beside it would be saying the same thing twice.
                if !spectrum {
                    ui::plate(ui, &palette, |ui| {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui::label(ui, &palette, t("input"));
                            ui.add(
                                egui::ProgressBar::new(self.level_peak.clamp(0.0, 1.0))
                                    .desired_width(STATUS_VISUALIZER_WIDTH)
                                    .desired_height(STATUS_VISUALIZER_HEIGHT)
                                    .show_percentage(),
                            )
                            .on_hover_text(hover);
                        });
                    });
                }

                if self.pressure == Pressure::Lagging {
                    ui::badge(
                        ui,
                        &palette,
                        t("transcription behind \u{2014} words arrive late"),
                        palette.warning,
                    );
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    service_pill(ui, &palette, self.voxtype_service);
                });
            });
        });
    }

    /// The newest notice, with the rest a click away.
    ///
    /// One line, because the top panel is above the transcript and a list of
    /// everything that ever went wrong would push the thing the user is
    /// reading off the screen.
    fn notice_panel(&mut self, ui: &mut egui::Ui) {
        if self.notices.is_empty() {
            self.notices_expanded = false;
            return;
        }
        let (newest_id, kind, text, count) = {
            let notice = self.notices.newest().expect("not empty");
            (notice.id, notice.kind, notice.text.clone(), notice.count)
        };
        let older = self.notices.len() - 1;

        let mut dismiss = false;
        let mut clear = false;
        egui::Panel::top("notices").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.add(notice_icon(kind).image(self.notice_color(kind), 14.0));
                let label = if count > 1 {
                    format!("{text}  ×{count}")
                } else {
                    text
                };
                ui.colored_label(self.notice_color(kind), label);
                if ui.small_button(t("Dismiss")).clicked() {
                    dismiss = true;
                }
                if older > 0 {
                    let more = format!("{older} {}", t("more"));
                    if ui.small_button(more).clicked() {
                        self.notices_expanded = !self.notices_expanded;
                    }
                }
            });
            if self.notices_expanded && older > 0 {
                egui::ScrollArea::vertical()
                    .id_salt("notice-history")
                    .max_height(140.0)
                    .show(ui, |ui| {
                        for notice in self.notices.iter_newest_first().skip(1) {
                            let line = if notice.count > 1 {
                                format!("{}  ×{}", notice.text, notice.count)
                            } else {
                                notice.text.clone()
                            };
                            ui.label(
                                RichText::new(line)
                                    .small()
                                    .color(self.notice_color(notice.kind)),
                            );
                        }
                    });
                if ui.small_button(t("Clear all")).clicked() {
                    clear = true;
                }
            }
        });

        if dismiss {
            self.notices.remove(newest_id);
            if self.capture_notice == Some(newest_id) {
                self.capture_notice = None;
            }
        }
        if clear {
            self.notices.clear();
            self.capture_notice = None;
            self.notices_expanded = false;
        }
    }

    fn notice_color(&self, kind: NoticeKind) -> egui::Color32 {
        match kind {
            NoticeKind::Info => self.palette.accent,
            NoticeKind::Warning => self.palette.warning,
            NoticeKind::Error => self.palette.danger,
        }
    }

    /// Re-reads the capture sources, keeping the user's choice selected if it
    /// is still there.
    ///
    /// An index into a list that has changed underneath is how a picker
    /// records the wrong thing, so the selection is matched by identity and
    /// nothing is selected when the chosen source is really gone.
    pub(super) fn refresh_sources(&mut self) {
        let chosen = self
            .selected_source
            .and_then(|index| self.sources.get(index))
            .cloned();
        let probe = crate::env::list_sources();
        if let Some(problem) = probe.problem {
            self.notify(NoticeKind::Warning, problem);
        }
        self.sources = probe.value;
        self.selected_source = reselect(chosen.as_ref(), &self.sources);
        if let (Some(previous), None) = (&chosen, self.selected_source) {
            self.notify(
                NoticeKind::Warning,
                tf(
                    "{} is not available any more, so nothing is selected to record.",
                    &[&previous.label()],
                ),
            );
        }
    }

    /// Capture devices offered as the microphone for the second track.
    pub(super) fn microphones(&self) -> Vec<AudioSource> {
        crate::env::microphones(&self.sources)
    }

    /// True while the pipeline is reading a source, when changing which source
    /// that is would have no effect until the next conversation.
    pub(super) fn source_locked(&self) -> bool {
        self.state != SessionState::Idle
    }

    /// Starts or stops the voxtype user service, then re-reads its real state
    /// rather than assuming the action worked.
    pub(super) fn set_service_running(&mut self, running: bool) {
        let outcome = if running {
            fc_voxtype::service::start()
        } else {
            fc_voxtype::service::stop()
        };
        if let Err(err) = outcome {
            // Two whole sentences rather than a verb substituted into one:
            // "could not {start} the service" is not a sentence a translator
            // can work with, because the verb inflects with what follows it.
            let message = if running {
                tf("Could not start voxtype.service: {}", &[&err.to_string()])
            } else {
                tf("Could not stop voxtype.service: {}", &[&err.to_string()])
            };
            self.notify(NoticeKind::Error, message);
        }
        self.voxtype_service = crate::env::service_status();
    }

    /// Keeps the palette in step with both the catalog and the user's choice.
    ///
    /// Two things can change it: the desktop's theme, which arrives through a
    /// background rescan, and the Appearance setting, which changes between
    /// one frame and the next. Watching only the first was enough while
    /// following the desktop was the only behaviour; now a changed setting has
    /// to be noticed too, which is what `theme_applied` records. Re-resolving
    /// unconditionally every frame would work and would also clone a palette
    /// and rebuild an `egui::Visuals` sixty times a second for nothing.
    fn poll_theme(&mut self, ctx: &egui::Context) {
        if self.theme_catalog.needs_reload() {
            self.theme_catalog
                .start(self.themes_dir.clone(), None, &self.theme_waker);
        }
        let rescanned = self.theme_catalog.poll();
        let wanted = self.wanted_theme();
        let chosen = self.theme_applied.as_ref() != Some(&wanted);
        if !rescanned && !chosen {
            return;
        }
        let Some(mut palette) = self.resolve_palette() else {
            return;
        };
        // A theme from the desktop, or from a file, has not been held to
        // anything.
        palette.enforce_contrast();
        self.theme_applied = Some(wanted);
        if palette != self.palette {
            self.palette = palette;
            self.palette.apply(ctx);
        }
    }

    /// Which theme should be in force right now.
    ///
    /// Compact mode may have one of its own. `None` there means "whatever the
    /// full window is using", which is the default; the two modes are the same
    /// window, so only one palette is ever live and switching modes
    /// re-resolves it.
    fn wanted_theme(&self) -> crate::theme::ThemeChoice {
        if self.compact {
            if let Some(theme) = &self.settings.compact_theme {
                return theme.clone();
            }
        }
        self.settings.theme.clone()
    }

    /// The palette the current [`crate::theme::ThemeChoice`] names.
    ///
    /// A named theme that is no longer in the directory falls back to the
    /// desktop's rather than to nothing: the file can be deleted or renamed
    /// between two launches, and an app that came up with stock egui grey
    /// because of it would look broken rather than out of date.
    fn resolve_palette(&self) -> Option<crate::theme::Palette> {
        use crate::theme::ThemeChoice;
        use fastframe_theme::{Base, Palette as _};

        let desktop = || {
            self.theme_catalog
                .system_theme()
                .or_else(|| self.theme_catalog.themes().first())
                .map(|theme| theme.palette.clone())
        };
        match &self.wanted_theme() {
            ThemeChoice::Neon => Some(crate::theme::Palette::neon()),
            ThemeChoice::System => desktop(),
            ThemeChoice::Dark => Some(crate::theme::Palette::base(Base::Dark)),
            ThemeChoice::Light => Some(crate::theme::Palette::base(Base::Light)),
            ThemeChoice::Named(filename) => self
                .theme_catalog
                .find(filename)
                .map(|theme| theme.palette.clone())
                .or_else(desktop),
        }
    }

    /// Drains tray events. `headless` is true while no window exists
    /// ([`fastframe_shell::Resident::headless_frame`]), when the only thing
    /// to do is remember that a window was asked for; with a window open,
    /// `Toggle`/`quit` act on it directly.
    fn drain_tray(&mut self, ctx: &egui::Context, headless: bool) {
        let Some(tray) = &mut self.tray else {
            return;
        };
        for event in tray.events() {
            match event {
                fastframe_tray::Event::Toggle | fastframe_tray::Event::Menu("show") => {
                    if headless {
                        self.wants_show = true;
                    } else {
                        ctx.send_viewport_cmd(ViewportCommand::Close);
                    }
                }
                fastframe_tray::Event::Show => {
                    if headless {
                        self.wants_show = true;
                    } else {
                        ctx.send_viewport_cmd(ViewportCommand::Focus);
                    }
                }
                fastframe_tray::Event::Menu("quit") => {
                    self.quit_requested = true;
                    if !headless {
                        ctx.send_viewport_cmd(ViewportCommand::Close);
                    }
                }
                fastframe_tray::Event::Menu(_) => {}
            }
        }
    }
}

impl fastframe_shell::Resident for App {
    fn closed(&self) -> fastframe_shell::Closed {
        if self.quit_requested || self.tray.is_none() {
            fastframe_shell::Closed::Quit
        } else {
            fastframe_shell::Closed::Hide
        }
    }

    fn window_gone(&mut self) {}

    fn headless_frame(&mut self, ctx: &egui::Context) -> fastframe_shell::Headless {
        self.drain_tray(ctx, true);
        if self.take_show_request() {
            self.wants_show = true;
        }
        if self.quit_requested {
            fastframe_shell::Headless::Quit
        } else if std::mem::take(&mut self.wants_show) {
            fastframe_shell::Headless::Show
        } else {
            fastframe_shell::Headless::Wait
        }
    }

    fn shutdown(&mut self) {
        if let Some(path) = self.session_config.take() {
            crate::env::remove_session_config(&path);
        }
        tracing::info!("fastcription shutting down");
    }
}

/// The source picker, plus the refresh that keeps it honest.
///
/// The list can legitimately be empty — no sound server running, `pactl`
/// missing, every device suspended — so nothing here indexes into it without
/// checking. Refresh matters because sink inputs come and go: the application
/// whose audio the user wants to transcribe may not have started playing when
/// fastcription launched.
///
/// Disabled while a session runs. The pipeline resolved its source when it
/// started and never looks again, so a picker that still moved would say
/// fastcription was recording something it was not.
pub(super) fn source_combo(app: &mut App, ui: &mut egui::Ui, id_salt: &str) {
    let locked = app.source_locked();
    let current = match app.selected_source.and_then(|index| app.sources.get(index)) {
        Some(source) => source.label(),
        None => t("No audio source").to_owned(),
    };
    ui.add_enabled_ui(!locked, |ui| {
        // The picker is the widest control on the bar and the one a reader
        // checks before pressing Start, so it is given the height of the row
        // rather than egui's default, which left it sitting in a dent.
        ui.spacing_mut().interact_size.y = crate::ui::CONTROL_HEIGHT;
        let combo = egui::ComboBox::from_id_salt(id_salt)
            // Elided here rather than by the widget: `ComboBox::width` sizes
            // the drop-down menu, not the button, so the button grew to the
            // sound server's full name — measured at 630 points for one HDMI
            // monitor, which pushed the transport off the edge of the window.
            .selected_text(elide(&current, COMBO_CHARS))
            .width(COMBO_WIDTH)
            .show_ui(ui, |ui| {
                if app.sources.is_empty() {
                    ui.label(t("Nothing to record"));
                }
                for index in 0..app.sources.len() {
                    let label = app.sources[index].label();
                    ui.selectable_value(&mut app.selected_source, Some(index), label);
                }
            });
        if locked {
            combo
                .response
                .on_disabled_hover_text(t("Stop the recording to change the source"));
        } else {
            // The full name, for the half of them the picker cannot show.
            combo.response.on_hover_text(current);
        }
    });
    let palette = app.palette.clone();
    if crate::ui::icon_button(ui, &palette, crate::icons::Icon::Reconnect, false)
        .on_hover_text(t("Look for audio sources again"))
        .clicked()
    {
        app.refresh_sources();
    }
}

/// The second-track toggle, disabled for the same reason the source picker is:
/// the mic track is opened when the session starts and not after.
pub(super) fn mic_checkbox(app: &mut App, ui: &mut egui::Ui, label: &str) {
    use crate::ui::{self, Tone};

    let locked = app.source_locked();
    let palette = app.palette.clone();
    let on = app.mic_track;
    // A pill that is lit when the track is on, rather than a checkbox. The bar
    // is a row of pills; one checkbox in the middle of it read as a leftover.
    let tone = if on { Tone::Primary } else { Tone::Normal };
    let tint = if on { palette.on_accent } else { palette.text };
    let response = ui
        .add_enabled_ui(!locked, |ui| {
            ui::pill(
                ui,
                &palette,
                tone,
                Some(crate::icons::Icon::Mic.image(tint, 14.0)),
                label,
            )
        })
        .inner;
    if locked {
        response.on_disabled_hover_text(t("Stop the recording to change the source"));
    } else {
        if response
            .on_hover_text(t("Also transcribe your own microphone, as a second track"))
            .clicked()
        {
            app.mic_track = !on;
        }
    }
}

/// How far into a galley to start so that no row is cut through its glyphs.
///
/// `hidden` is how much of the galley's height has to go, measured from its
/// top. The answer is the top of the first row that begins at or after that,
/// which is always a clean boundary. A galley with no rows, or one whose last
/// row still starts before the cut, falls back to hiding exactly what was
/// asked — there is nothing better to do, and it is no worse than the clip
/// would have been on its own.
fn first_visible_row(galley: &egui::Galley, hidden: f32) -> f32 {
    galley
        .rows
        .iter()
        .map(|row| row.pos.y)
        .find(|&y| y >= hidden)
        .unwrap_or(hidden)
}

/// A filled circle, for the state light on the top bar.
///
/// `lit` gives it the halo. Only the recording state gets one: a glow that is
/// always there says nothing, and the point of this dot is to be the thing
/// that changes.
fn dot(color: egui::Color32, lit: bool) -> impl egui::Widget {
    move |ui: &mut egui::Ui| {
        let size = egui::Vec2::splat(10.0);
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            if lit {
                for (grow, alpha) in [(10.0_f32, 0.10_f32), (6.0, 0.18), (3.0, 0.32)] {
                    painter.circle_filled(
                        rect.center(),
                        rect.width() / 2.0 + grow,
                        color.gamma_multiply(alpha),
                    );
                }
            }
            painter.circle_filled(rect.center(), rect.width() / 2.0, color);
        }
        response
    }
}

/// A hairline between two readouts, shorter than the row so it reads as a
/// divider rather than as a border.
fn separator(ui: &mut egui::Ui, palette: &crate::theme::Palette) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(1.0, 24.0), egui::Sense::hover());
    ui.painter().rect_filled(
        egui::Rect::from_center_size(rect.center(), egui::vec2(1.0, 24.0)),
        0,
        palette.outline,
    );
}

fn service_pill(ui: &mut egui::Ui, palette: &crate::theme::Palette, status: ServiceStatus) {
    // Palette colours, not literals: hardcoded hex stayed dark-theme green and
    // red whatever the desktop theme said, and on a light palette the "running"
    // green was unreadable against the panel.
    let (icon, text, color) = match status {
        ServiceStatus::Running => (
            crate::icons::Icon::StatusOk,
            t("voxtype: running"),
            palette.accent,
        ),
        ServiceStatus::Stopped => (
            crate::icons::Icon::StatusWarn,
            t("voxtype: stopped"),
            palette.warning,
        ),
        ServiceStatus::Unknown => (
            crate::icons::Icon::StatusWarn,
            t("voxtype: unknown"),
            palette.dim,
        ),
    };
    // One capsule rather than an icon and a label side by side: aligned to the
    // right of the bar, the two used to be laid out in reverse and needed a
    // comment explaining why the label came first.
    let _ = icon;
    crate::ui::badge(ui, palette, text, color);
}

fn notice_icon(kind: NoticeKind) -> crate::icons::Icon {
    match kind {
        NoticeKind::Info => crate::icons::Icon::StatusOk,
        NoticeKind::Warning | NoticeKind::Error => crate::icons::Icon::StatusWarn,
    }
}

/// A flat purple disc: fastcription has no app icon yet, so the tray draws
/// one procedurally rather than shipping a placeholder asset.
pub(super) fn tray_icon_rgba(size: usize) -> Vec<u8> {
    let mut pixels = vec![0u8; size * size * 4];
    let radius = size as f32 / 2.0;
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 + 0.5 - radius;
            let dy = y as f32 + 0.5 - radius;
            if (dx * dx + dy * dy).sqrt() <= radius * 0.9 {
                let index = (y * size + x) * 4;
                pixels[index] = 0x8a;
                pixels[index + 1] = 0x7a;
                pixels[index + 2] = 0xe8;
                pixels[index + 3] = 0xff;
            }
        }
    }
    pixels
}

/// Which key was pressed, as the transport understands it. Three keys, because
/// one of them has to mean two things: the key that starts a recording is the
/// key that resumes a paused one, since from the reader's side those are the
/// same request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TransportKey {
    /// `Ctrl+R`.
    StartOrResume,
    /// `Ctrl+Space`.
    PauseOrResume,
    /// `Ctrl+.`
    Stop,
}

/// What the session should be asked to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Transport {
    Start,
    Pause,
    Resume,
    Stop,
}

/// Maps a key onto an action, or onto nothing.
///
/// Pure, so the thing that is easy to get wrong — which key does what in which
/// state, and above all that nothing fires while a session is `Finishing` — is
/// tested instead of being spread across the branches of a frame. Starting a
/// second session while the first is still transcribing its backlog would take
/// the store's conversation row out from under it.
pub(super) fn transport_for(key: TransportKey, state: SessionState) -> Option<Transport> {
    match (key, state) {
        (TransportKey::StartOrResume, SessionState::Idle) => Some(Transport::Start),
        (TransportKey::StartOrResume, SessionState::Paused)
        | (TransportKey::PauseOrResume, SessionState::Paused) => Some(Transport::Resume),
        (TransportKey::PauseOrResume, SessionState::Recording) => Some(Transport::Pause),
        (TransportKey::Stop, SessionState::Recording | SessionState::Paused) => {
            Some(Transport::Stop)
        }
        _ => None,
    }
}

/// Shortens a label to fit a fixed-width control, keeping both ends.
///
/// A source name carries its meaning at both ends — "Monitor of" at the front
/// and the device at the back, as in "Monitor of 800 Series … (HDMI)
/// [U28E590]" — so the middle is what goes.
pub(super) fn elide(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars || max_chars < 5 {
        return text.to_owned();
    }
    let keep = max_chars - 1;
    let head = keep.div_ceil(2);
    let tail = keep - head;
    let chars: Vec<char> = text.chars().collect();
    let front: String = chars[..head].iter().collect();
    let back: String = chars[count - tail..].iter().collect();
    format!("{}\u{2026}{}", front.trim_end(), back.trim_start())
}

/// What to select when nothing was remembered.
///
/// The monitor of the default sink is what the user is actually hearing, so it
/// is the source a meeting needs; any sink monitor is the next best, and a
/// capture device would transcribe the user's own room. Falls back to the
/// first entry so a machine with a single source of any kind is ready to
/// start, and to nothing at all when there is nothing to record.
pub(super) fn default_selection(
    sources: &[AudioSource],
    default_monitor: Option<&str>,
) -> Option<usize> {
    default_monitor
        .and_then(|name| sources.iter().position(|source| source.name == name))
        .or_else(|| {
            sources
                .iter()
                .position(|source| source.kind == fc_core::SourceKind::SinkMonitor)
        })
        .or_else(|| (!sources.is_empty()).then_some(0))
}

/// Finds the user's previously chosen source in a freshly enumerated list.
///
/// Matched on identity rather than position: a source that disappears shifts
/// every index after it. `None` when the previous choice is gone — the caller
/// then selects nothing and says so. Falling back to the first entry, which is
/// what this used to do, meant a meeting could be recorded from whatever
/// happened to be listed first, which is the worst failure this app has.
pub(super) fn reselect(previous: Option<&AudioSource>, sources: &[AudioSource]) -> Option<usize> {
    let previous = previous?;
    sources.iter().position(|source| {
        source.kind == previous.kind
            && source.name == previous.name
            && source.application == previous.application
    })
}

#[cfg(test)]
mod tests {
    use super::{default_selection, elide, reselect, transport_for, Transport, TransportKey};
    use fc_core::{AudioSource, SessionState, SourceKind};

    /// One key starts and resumes, because a reader who paused to answer the
    /// door presses the same thing to carry on.
    #[test]
    fn the_start_key_resumes_a_paused_session() {
        let key = TransportKey::StartOrResume;
        assert_eq!(
            transport_for(key, SessionState::Idle),
            Some(Transport::Start)
        );
        assert_eq!(
            transport_for(key, SessionState::Paused),
            Some(Transport::Resume)
        );
        // Already recording: pressing it again must not start a second session.
        assert_eq!(transport_for(key, SessionState::Recording), None);
    }

    #[test]
    fn the_pause_key_toggles() {
        let key = TransportKey::PauseOrResume;
        assert_eq!(
            transport_for(key, SessionState::Recording),
            Some(Transport::Pause)
        );
        assert_eq!(
            transport_for(key, SessionState::Paused),
            Some(Transport::Resume)
        );
        assert_eq!(transport_for(key, SessionState::Idle), None);
    }

    #[test]
    fn stopping_needs_something_to_stop() {
        let key = TransportKey::Stop;
        assert_eq!(
            transport_for(key, SessionState::Recording),
            Some(Transport::Stop)
        );
        assert_eq!(
            transport_for(key, SessionState::Paused),
            Some(Transport::Stop)
        );
        assert_eq!(transport_for(key, SessionState::Idle), None);
    }

    /// `Finishing` is capture stopped with a backlog still being transcribed.
    /// Every key has to be inert there: starting would steal the conversation
    /// row the session is still appending to, and stopping twice would hand the
    /// same session off to finish on two threads.
    #[test]
    fn nothing_fires_while_a_session_is_finishing() {
        for key in [
            TransportKey::StartOrResume,
            TransportKey::PauseOrResume,
            TransportKey::Stop,
        ] {
            assert_eq!(transport_for(key, SessionState::Finishing), None, "{key:?}");
        }
    }

    fn monitor(name: &str) -> AudioSource {
        AudioSource::named(SourceKind::SinkMonitor, name, name)
    }

    #[test]
    fn a_source_name_is_elided_in_the_middle_keeping_both_ends() {
        let long = "Monitor of 800 Series Chipset Family Audio Context Engine (ACE) Digital Stereo (HDMI) [U28E590]";
        let short = elide(long, 34);
        assert_eq!(short.chars().count(), 34);
        assert!(short.starts_with("Monitor of"), "{short}");
        assert!(short.ends_with("[U28E590]"), "{short}");
        assert!(short.contains('\u{2026}'));
        // Short enough already: left exactly alone.
        assert_eq!(elide("Built-in Audio", 34), "Built-in Audio");
        // Multi-byte text must not be split through a character.
        let cjk = "会議の音声をここから取り込みます、とても長い名前です";
        assert_eq!(elide(cjk, 10).chars().count(), 10);
    }

    #[test]
    fn a_fresh_start_prefers_a_monitor_then_anything_then_nothing() {
        let mic = AudioSource::named(SourceKind::Device, "mic", "USB Microphone");
        let mon = monitor("speakers.monitor");
        assert_eq!(
            default_selection(&[mic.clone(), mon.clone()], None),
            Some(1)
        );
        assert_eq!(default_selection(std::slice::from_ref(&mic), None), Some(0));
        assert_eq!(default_selection(&[], None), None);
        // The default sink's monitor wins over an earlier monitor in the list.
        let hdmi = monitor("hdmi.monitor");
        assert_eq!(
            default_selection(&[hdmi, mon.clone()], Some("speakers.monitor")),
            Some(1)
        );
        // A default that is not in the list falls back to the first monitor.
        assert_eq!(
            default_selection(&[mic, mon], Some("ghost.monitor")),
            Some(1)
        );
    }

    #[test]
    fn a_refresh_keeps_the_chosen_source_when_the_list_shifts() {
        let previous = monitor("b.monitor");
        let after = vec![
            monitor("new.monitor"),
            monitor("a.monitor"),
            monitor("b.monitor"),
        ];
        assert_eq!(reselect(Some(&previous), &after), Some(2));
    }

    /// A source that went away selects nothing. It used to select index 0,
    /// which on a machine whose first entry is a different sink means pressing
    /// Start records the wrong conversation.
    #[test]
    fn a_vanished_source_selects_nothing() {
        let previous = monitor("gone.monitor");
        let after = vec![monitor("a.monitor")];
        assert_eq!(reselect(Some(&previous), &after), None);
    }

    #[test]
    fn an_empty_list_selects_nothing_without_panicking() {
        let previous = monitor("a.monitor");
        assert_eq!(reselect(Some(&previous), &[]), None);
        assert_eq!(reselect(None, &[]), None);
    }

    /// Two streams of the same application are told apart by index, which the
    /// identity match deliberately ignores — so the application name has to be
    /// part of what is compared, or the wrong one gets picked.
    #[test]
    fn sink_inputs_match_on_application_not_position() {
        let firefox = AudioSource::sink_input(12, "Firefox", "Firefox playback");
        let after = vec![
            AudioSource::sink_input(30, "Spotify", "Spotify playback"),
            AudioSource::sink_input(31, "Firefox", "Firefox playback"),
        ];
        assert_eq!(reselect(Some(&firefox), &after), Some(1));
    }
}
