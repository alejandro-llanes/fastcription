//! The window itself: what is re-applied to every fresh `egui::Context`, the
//! frame that lays the panels out, the top bar, the notices, compact mode, the
//! tray and the theme.
//!
//! Split out of `app/mod.rs` so the chrome — the part that has to run again on
//! every reopened window — can be read without the pipeline and the library in
//! the way.

use std::time::Instant;

use egui::{RichText, ViewportCommand, WindowLevel};
use fc_core::{AudioSource, SessionState};

use crate::app::{session_control::format_elapsed, App, MainView, NoticeKind, ServiceStatus};
use crate::i18n::t;

/// The window's ordinary title, and the one it takes in compact mode.
///
/// The compact title is what a compositor rule matches on (`docs/OVERLAY.md`),
/// so it is a contract with the user's configuration, not decoration.
const TITLE: &str = "fastcription";
pub(super) const COMPACT_TITLE: &str = "fastcription — captions";

/// Compact mode's window size: wide enough for a sentence at 22 pt, short
/// enough to sit under a video call without covering a face.
const COMPACT_SIZE: [f32; 2] = [760.0, 170.0];

/// How many committed lines compact mode shows above the live one.
const COMPACT_LINES: usize = 4;

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

        if self.compact {
            self.compact_frame(ui);
            return;
        }

        self.delete_confirmation(&ctx);

        egui::Panel::top("top-bar").show(ui, |ui| self.top_bar(ui));
        self.notice_panel(ui);

        egui::Panel::left("sidebar")
            .resizable(true)
            .default_size(260.0)
            .size_range(200.0..=420.0)
            .show(ui, |ui| super::sidebar::show(self, ui));

        egui::CentralPanel::default().show(ui, |ui| match self.main_view {
            MainView::Live => super::live::show(self, ui),
            MainView::History(id) => super::history::show(self, ui, id),
            MainView::Settings => super::settings::show(self, ui),
        });
    }

    /// `Ctrl+Shift+C` toggles compact mode, `Esc` leaves it.
    ///
    /// `Esc` only works while the window has focus, which is the point: a
    /// compositor rule that keeps the captions above a call usually also keeps
    /// focus in the call, and then nothing here sees the key at all — hence
    /// the restore button compact mode draws for itself.
    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let toggle = egui::KeyboardShortcut::new(
            egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
            egui::Key::C,
        );
        if ctx.input_mut(|i| i.consume_shortcut(&toggle)) {
            self.set_compact(ctx, !self.compact);
        }
        if self.compact {
            let focused = ctx.input(|i| i.focused);
            if focused && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.set_compact(ctx, false);
            }
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
        if compact {
            // `viewport_rect` rather than `viewport().inner_rect`: on Wayland
            // the compositor never tells a client where its window is, so the
            // viewport info's rect is `None` and this is the only size there is.
            self.restore_size = Some(ctx.viewport_rect().size());
            self.apply_compact(ctx);
        } else {
            let size = self
                .restore_size
                .take()
                .unwrap_or(egui::vec2(1180.0, 760.0));
            ctx.send_viewport_cmd(ViewportCommand::Decorations(true));
            ctx.send_viewport_cmd(ViewportCommand::WindowLevel(WindowLevel::Normal));
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
            ctx.send_viewport_cmd(ViewportCommand::Title(TITLE.to_owned()));
        }
    }

    fn apply_compact(&self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(COMPACT_SIZE.into()));
        ctx.send_viewport_cmd(ViewportCommand::Decorations(false));
        ctx.send_viewport_cmd(ViewportCommand::WindowLevel(WindowLevel::AlwaysOnTop));
        ctx.send_viewport_cmd(ViewportCommand::Title(COMPACT_TITLE.to_owned()));
    }

    /// Captions and nothing else: the last few committed lines, then the one
    /// being spoken, dimmed because its tail is still being revised.
    fn compact_frame(&mut self, ui: &mut egui::Ui) {
        let size = self.settings.transcript_pt.clamp(14.0, 48.0);
        let mut leave = false;
        egui::CentralPanel::default()
            .frame(
                egui::Frame::default()
                    .fill(self.palette.window)
                    .inner_margin(12.0),
            )
            .show(ui, |ui| {
                egui::Panel::bottom("compact-controls").show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format_elapsed(self.elapsed()))
                                .small()
                                .color(self.palette.dim),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            leave = ui
                                .small_button(t("Restore"))
                                .on_hover_text(t("Back to the full window (Esc)"))
                                .clicked();
                        });
                    });
                });

                egui::ScrollArea::vertical()
                    .id_salt("compact-transcript")
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        let hidden = self.segments.len().saturating_sub(COMPACT_LINES);
                        for segment in self.segments.iter().skip(hidden) {
                            ui.label(
                                RichText::new(&segment.text)
                                    .size(size)
                                    .color(self.palette.text),
                            );
                        }
                        let mut pending: Vec<&fc_core::Segment> =
                            self.provisional.values().collect();
                        pending.sort_by_key(|segment| segment.seq);
                        for segment in pending {
                            // The in-flight tail is replaced on every pass, so
                            // it is shown as provisional rather than as text
                            // the user can rely on having been said.
                            ui.label(
                                RichText::new(&segment.text)
                                    .size(size)
                                    .color(self.palette.dim),
                            );
                        }
                        if self.segments.is_empty() && self.provisional.is_empty() {
                            ui.label(
                                RichText::new(t("Waiting for speech…"))
                                    .size(size)
                                    .color(self.palette.dim),
                            );
                        }
                    });
            });
        if leave {
            let ctx = ui.ctx().clone();
            self.set_compact(&ctx, false);
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui
                .selectable_label(self.main_view == MainView::Live, t("Live"))
                .clicked()
            {
                self.main_view = MainView::Live;
            }
            ui.separator();

            source_combo(self, ui, "top-bar-source");

            ui.separator();
            ui.add(crate::icons::Icon::Mic.image(self.palette.secondary, 14.0));
            ui.add(
                egui::ProgressBar::new(self.level_peak.clamp(0.0, 1.0))
                    .desired_width(90.0)
                    .show_percentage(),
            );

            ui.separator();
            let can_start = matches!(self.state, SessionState::Idle | SessionState::Paused)
                && !self.library_read_only;
            let start_label = if self.state == SessionState::Paused {
                t("Resume")
            } else {
                t("Start")
            };
            let start = ui.add_enabled(can_start, egui::Button::new(start_label));
            if self.library_read_only {
                start.on_disabled_hover_text(format!(
                    "{} {}",
                    t("The conversation library is read-only, so nothing can be recorded:"),
                    self.library_path.display()
                ));
            } else if start.clicked() {
                self.start_or_resume();
            }
            if ui
                .add_enabled(
                    self.state == SessionState::Recording,
                    egui::Button::new(t("Pause")),
                )
                .clicked()
            {
                self.pause();
            }
            if ui
                .add_enabled(
                    matches!(self.state, SessionState::Recording | SessionState::Paused),
                    egui::Button::new(t("Stop")),
                )
                .clicked()
            {
                self.stop();
            }

            ui.separator();
            ui.label(format_elapsed(self.elapsed()));

            ui.separator();
            mic_checkbox(self, ui, t("Mic"));

            ui.separator();
            service_pill(ui, &self.palette, self.voxtype_service);

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .selectable_label(self.main_view == MainView::Settings, t("Settings"))
                    .clicked()
                {
                    self.main_view = MainView::Settings;
                }
                if ui
                    .selectable_label(self.compact, t("Compact"))
                    .on_hover_text(t(
                        "Captions only, always on top (Ctrl+Shift+C). Esc comes back.",
                    ))
                    .clicked()
                {
                    let ctx = ui.ctx().clone();
                    self.set_compact(&ctx, true);
                }
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
                format!(
                    "{} is not available any more, so nothing is selected to record.",
                    previous.label()
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
            let verb = if running { "start" } else { "stop" };
            self.notify(
                NoticeKind::Error,
                format!("Could not {verb} voxtype.service: {err}"),
            );
        }
        self.voxtype_service = crate::env::service_status();
    }

    fn poll_theme(&mut self, ctx: &egui::Context) {
        if self.theme_catalog.needs_reload() {
            self.theme_catalog
                .start(self.themes_dir.clone(), None, &self.theme_waker);
        }
        if self.theme_catalog.poll() {
            let next = self
                .theme_catalog
                .system_theme()
                .or_else(|| self.theme_catalog.themes().first())
                .map(|theme| theme.palette.clone());
            if let Some(palette) = next {
                self.palette = palette;
                self.palette.apply(ctx);
            }
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
        let combo = egui::ComboBox::from_id_salt(id_salt)
            .selected_text(current)
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
        }
    });
    if ui
        .small_button(t("↻"))
        .on_hover_text(t("Look for audio sources again"))
        .clicked()
    {
        app.refresh_sources();
    }
}

/// The second-track toggle, disabled for the same reason the source picker is:
/// the mic track is opened when the session starts and not after.
pub(super) fn mic_checkbox(app: &mut App, ui: &mut egui::Ui, label: &str) {
    let locked = app.source_locked();
    let response = ui
        .add_enabled_ui(!locked, |ui| ui.checkbox(&mut app.mic_track, label))
        .inner;
    if locked {
        response.on_disabled_hover_text(t("Stop the recording to change the source"));
    }
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
    ui.add(icon.image(color, 14.0));
    ui.colored_label(color, text);
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
    use super::reselect;
    use fc_core::{AudioSource, SourceKind};

    fn monitor(name: &str) -> AudioSource {
        AudioSource::named(SourceKind::SinkMonitor, name, name)
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
