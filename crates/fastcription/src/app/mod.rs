//! App state, the `SessionEvent` channel, and the frame that ties the chrome
//! together. Everything lives in one `App`: the real pipeline, when it
//! exists, replaces `crate::demo` and nothing else here changes.

mod export_ui;
mod history;
mod live;
mod overlay;
mod settings;
mod sidebar;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::Receiver;
use fastframe_theme::Palette as ThemePalette;

use fc_core::{
    AudioSource, Conversation, ConversationId, EngineInfo, Group,
    Pressure, Segment, SessionEvent, SessionState, Tag, TagId, Track,
};

use crate::session::{self, Session, SessionConfig, SharedStore};

use crate::i18n::t;

/// Which main-pane view is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MainView {
    Live,
    History(ConversationId),
    Settings,
}

/// Stands in for the voxtype systemd user service's state (ARCHITECTURE.md
/// §7) until `fc-voxtype::service` is wired into the settings pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceStatus {
    Unknown,
    Running,
    Stopped,
}

/// A dismissible error banner, raised by `SessionEvent::Failed` or
/// `SourceLost`.
struct Banner {
    message: String,
}

pub struct App {
    // Chrome: fastframe wiring that needs to run again on every reopened
    // window, since each one gets its own fresh `egui::Context`.
    theme_catalog: fastframe_theme::Catalog<crate::theme::Palette>,
    theme_waker: fastframe_theme::Waker,
    themes_dir: PathBuf,
    palette: crate::theme::Palette,
    text_rendering: fastframe_text::TextRendering,
    scrolling: fastframe_scroll::Scrolling,

    // Tray + shell: `closed()` hides to the tray instead of quitting when one
    // exists; `wants_show`/`quit_requested` cross from tray events (which can
    // arrive with no window open) into `Resident::headless_frame`.
    tray: Option<fastframe_tray::Tray>,
    wants_show: bool,
    quit_requested: bool,

    // The pipeline. `App` only ever drains `SessionEvent`s off `events` and
    // calls pause/resume/stop on `session`; it knows nothing about capture or
    // transcription. `events` is a handle to the running session's channel,
    // held separately so the tail of a finishing session still arrives after
    // `Session` itself has been consumed by `stop_async`.
    store: Option<SharedStore>,
    captures: session::CaptureFactory,
    voxtype: Option<PathBuf>,
    engine: EngineInfo,
    session: Option<Session>,
    events: Option<Receiver<SessionEvent>>,

    // Live session state
    state: SessionState,
    session_start: Option<Instant>,
    accumulated: Duration,
    pressure: Pressure,
    level_peak: f32,
    level_rms: f32,
    mic_track: bool,
    sources: Vec<AudioSource>,
    selected_source: usize,
    banner: Option<Banner>,
    segments: Vec<Segment>,
    provisional: HashMap<Track, Segment>,
    overlay: overlay::Shared,
    overlay_open: bool,

    // Library (placeholder data; `fc-store` owns the real rows)
    conversations: Vec<Conversation>,
    groups: Vec<Group>,
    tags: Vec<Tag>,
    conversation_tags: HashMap<ConversationId, Vec<TagId>>,
    history_segments: HashMap<ConversationId, Vec<Segment>>,

    // Navigation and per-pane transient state
    main_view: MainView,
    sidebar: sidebar::State,
    settings: settings::State,
    export: export_ui::State,

    voxtype_service: ServiceStatus,
}

impl App {
    pub fn new(shell_waker: &fastframe_shell::Waker) -> Self {
        let store = crate::env::open_store();
        let sources = crate::env::list_sources();
        let voxtype = fc_voxtype::cli::find_binary();
        let engine = crate::env::probe_engine(voxtype.as_ref());
        let library = store
            .value
            .as_ref()
            .map(crate::env::load_library)
            .unwrap_or_else(|| crate::env::Probe {
                value: crate::env::Library::default(),
                problem: None,
            });
        if voxtype.is_none() {
            tracing::warn!("voxtype was not found on PATH; recording will fail until it is installed");
        }
        let banner = [store.problem, sources.problem, library.problem]
            .into_iter()
            .flatten()
            .next()
            .map(|message| Banner { message });

        let themes_dir = dirs::config_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("fastcription")
            .join("themes");
        let mut theme_catalog = fastframe_theme::Catalog::default();
        theme_catalog.enable_desktop_themes(fastframe_theme::DesktopThemes {
            slug: "fastcription",
            omarchy_template: fastframe_theme::omarchy::BASE_TEMPLATE,
            omarchy_previous_templates: &[],
            presets: true,
        });

        let tray = {
            let waker = shell_waker.clone();
            fastframe_tray::Tray::spawn(
                fastframe_tray::Config {
                    id: "fastcription",
                    title: "fastcription".to_owned(),
                    icon: tray_icon_rgba,
                    template_icon: None,
                    themed_icon: false,
                    menu_on_click: false,
                    menu: vec![
                        fastframe_tray::MenuItem::action("show", t("Show fastcription")),
                        fastframe_tray::MenuItem::Separator,
                        fastframe_tray::MenuItem::action("quit", t("Quit")),
                    ],
                },
                move || waker.wake(),
            )
        };

        Self {
            theme_catalog,
            theme_waker: fastframe_theme::Waker::default(),
            themes_dir,
            palette: ThemePalette::base(fastframe_theme::Base::Dark),
            text_rendering: fastframe_text::detect(),
            scrolling: fastframe_scroll::Scrolling::default(),
            tray,
            wants_show: false,
            quit_requested: false,
            store: store.value,
            captures: session::parec_captures(),
            voxtype,
            engine,
            session: None,
            events: None,
            state: SessionState::Idle,
            session_start: None,
            accumulated: Duration::ZERO,
            pressure: Pressure::Keeping,
            level_peak: 0.0,
            level_rms: 0.0,
            mic_track: false,
            sources: sources.value,
            selected_source: 0,
            banner,
            segments: Vec::new(),
            provisional: HashMap::new(),
            overlay: Arc::new(Mutex::new(overlay::State::default())),
            overlay_open: false,
            conversations: library.value.conversations,
            groups: library.value.groups,
            tags: library.value.tags,
            conversation_tags: library.value.conversation_tags,
            history_segments: HashMap::new(),
            main_view: MainView::Live,
            sidebar: sidebar::State::default(),
            settings: settings::State::default(),
            export: export_ui::State::default(),
            voxtype_service: crate::env::service_status(),
        }
    }

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
    }

    /// The whole window's content for one pass. `ui` is the viewport's root
    /// `Ui` (see this eframe fork's `App::ui`, not the usual `update`).
    pub fn frame(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.scrolling.apply(ui.ctx());
        self.drain_events(ui.ctx());
        self.poll_theme(ui.ctx());
        self.drain_tray(ui.ctx(), false);
        if self.quit_requested {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }

        egui::Panel::top("top-bar").show(ui, |ui| self.top_bar(ui));

        if let Some(message) = self.banner.as_ref().map(|banner| banner.message.clone()) {
            let dismiss = egui::Panel::top("banner")
                .show(ui, |ui| {
                    let mut dismiss = false;
                    ui.horizontal(|ui| {
                        ui.colored_label(self.palette.danger, &message);
                        if ui.small_button(t("Dismiss")).clicked() {
                            dismiss = true;
                        }
                    });
                    dismiss
                })
                .inner;
            if dismiss {
                self.banner = None;
            }
        }

        egui::Panel::left("sidebar")
            .resizable(true)
            .default_size(260.0)
            .size_range(200.0..=420.0)
            .show(ui, |ui| sidebar::show(self, ui));

        egui::CentralPanel::default().show(ui, |ui| match self.main_view {
            MainView::Live => live::show(self, ui),
            MainView::History(id) => history::show(self, ui, id),
            MainView::Settings => settings::show(self, ui),
        });

        if self.overlay_open {
            overlay::show(ui.ctx(), &self.overlay);
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
            let can_start = matches!(self.state, SessionState::Idle | SessionState::Paused);
            let start_label = if self.state == SessionState::Paused {
                t("Resume")
            } else {
                t("Start")
            };
            if ui
                .add_enabled(can_start, egui::Button::new(start_label))
                .clicked()
            {
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
            ui.checkbox(&mut self.mic_track, t("Mic"));

            ui.separator();
            service_pill(ui, self.voxtype_service);

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .selectable_label(self.main_view == MainView::Settings, t("Settings"))
                    .clicked()
                {
                    self.main_view = MainView::Settings;
                }
                if ui
                    .selectable_label(self.overlay_open, t("Overlay"))
                    .clicked()
                {
                    self.overlay_open = !self.overlay_open;
                }
            });
        });
    }

    fn set_state(&mut self, new: SessionState) {
        match (self.state, new) {
            (SessionState::Idle, SessionState::Recording)
            | (SessionState::Paused, SessionState::Recording) => {
                self.session_start = Some(Instant::now());
            }
            (SessionState::Recording, SessionState::Paused) => {
                if let Some(start) = self.session_start.take() {
                    self.accumulated += start.elapsed();
                }
            }
            (SessionState::Finishing, SessionState::Idle) => {
                self.session_start = None;
                self.accumulated = Duration::ZERO;
                self.level_peak = 0.0;
                self.level_rms = 0.0;
                self.provisional.clear();
                self.state = new;
                // The conversation row is closed by now, so the sidebar can
                // show it with its real duration and segment count.
                self.reload_library();
                return;
            }
            (_, SessionState::Idle) => {
                self.session_start = None;
                self.accumulated = Duration::ZERO;
                self.level_peak = 0.0;
                self.level_rms = 0.0;
                self.provisional.clear();
            }
            _ => {}
        }
        self.state = new;
    }

    /// Starts a new conversation, or resumes the paused one.
    fn start_or_resume(&mut self) {
        if let Some(session) = &self.session {
            session.resume();
            return;
        }

        let Some(store) = self.store.clone() else {
            self.raise("Recording is disabled because the conversation library could not be opened.");
            return;
        };
        let Some(chosen) = self.sources.get(self.selected_source).cloned() else {
            self.raise("Choose an audio source first.");
            return;
        };

        // Re-resolved rather than trusted: a sink-input index goes stale when
        // the application that owned it restarts, and recording the wrong
        // stream is worse than refusing.
        let source = match fc_audio::resolve(&chosen) {
            Ok(source) => source,
            Err(err) => {
                self.raise(format!("{} is not available: {err}", chosen.label()));
                return;
            }
        };

        let config = SessionConfig {
            title: default_title(),
            group: None,
            source,
            mic_source: self.mic_track.then(crate::env::default_microphone),
            segmenter: self.segmenter_config(),
            engine: self.engine.clone(),
        };

        match Session::start(
            store,
            config,
            self.transcriber_factory(),
            &self.captures,
            now_millis(),
        ) {
            Ok(session) => {
                self.segments.clear();
                self.provisional.clear();
                overlay::sync(&self.overlay, &self.segments);
                self.banner = None;
                self.events = Some(session.events.clone());
                self.session = Some(session);
                self.set_state(SessionState::Recording);
            }
            Err(err) => self.raise(format!("Could not start recording: {err}")),
        }
    }

    fn pause(&mut self) {
        if let Some(session) = &self.session {
            session.pause();
        }
    }

    /// Hands the session off to finish on its own thread. `Finishing` shows
    /// immediately; the session reports `Idle` once the backlog is transcribed
    /// and the conversation is closed, and the library is reloaded then.
    fn stop(&mut self) {
        if let Some(session) = self.session.take() {
            self.set_state(SessionState::Finishing);
            session.stop_async(now_millis());
        }
    }

    /// Builds the per-track transcriber.
    ///
    /// Engine, model and language are deliberately not passed: voxtype reads
    /// its own configuration, and pinning what was probed at startup would
    /// freeze the user's choice for the lifetime of the window (decision D6).
    fn transcriber_factory(&self) -> session::TranscriberFactory {
        let binary = self.voxtype.clone();
        Box::new(move |_track| {
            let mut cli = fc_asr::VoxtypeCli::new();
            if let Some(path) = &binary {
                cli = cli.with_binary(path.display().to_string());
            }
            Box::new(cli)
        })
    }

    fn segmenter_config(&self) -> fc_asr::SegmenterConfig {
        let defaults = fc_asr::SegmenterConfig::default();
        let target_ms = (self.settings.chunk_target_secs.max(1.0) * 1_000.0) as u64;
        let max_chunk_ms = (self.settings.chunk_max_secs.max(2.0) * 1_000.0) as u64;
        fc_asr::SegmenterConfig {
            // The ladder's first rung is the target the user set; the rest grow
            // from it so backpressure still has somewhere to go.
            growth_ladder_ms: vec![target_ms, target_ms * 3 / 2, max_chunk_ms.max(target_ms)],
            max_chunk_ms: max_chunk_ms.max(target_ms),
            ..defaults
        }
    }

    /// Writes an edited conversation back to the library.
    ///
    /// The history pane edits its own copy and hands it back; this is where
    /// that copy becomes durable. A failure raises a banner rather than being
    /// swallowed, because silently losing a rename is the kind of bug users
    /// cannot diagnose.
    fn persist_conversation(&mut self, conversation: &Conversation) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let guard = match store.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Err(err) = guard.rename_conversation(conversation.id, &conversation.title) {
            drop(guard);
            self.raise(format!("Could not rename the conversation: {err}"));
            return;
        }
        if let Err(err) = guard.set_group(conversation.id, conversation.group) {
            drop(guard);
            self.raise(format!("Could not change the group: {err}"));
        }
    }

    /// Applies a tag change for one conversation.
    fn persist_tags(&mut self, id: ConversationId, tags: &[TagId]) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let existing: Vec<TagId> = self
            .conversation_tags
            .get(&id)
            .cloned()
            .unwrap_or_default();
        let guard = match store.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let mut failure = None;
        for added in tags.iter().filter(|t| !existing.contains(t)) {
            if let Err(err) = guard.add_tag(id, *added) {
                failure = Some(err.to_string());
            }
        }
        for removed in existing.iter().filter(|t| !tags.contains(t)) {
            if let Err(err) = guard.remove_tag(id, *removed) {
                failure = Some(err.to_string());
            }
        }
        drop(guard);
        if let Some(message) = failure {
            self.raise(format!("Could not update tags: {message}"));
        }
    }

    /// Writes a transcript to a file next to the user's other downloads.
    ///
    /// There is no file dialog: pulling in a portal dependency for this is not
    /// worth it yet, so the path is chosen here and reported back so the user
    /// knows exactly where it went.
    fn export_conversation(
        &mut self,
        conversation: &Conversation,
        format: fc_export::ExportFormat,
        options: fc_export::ExportOptions,
    ) {
        let segments = self.segments_for(conversation.id).to_vec();
        if segments.is_empty() {
            self.raise("That conversation has no transcript to export.");
            return;
        }

        let rendered = fc_export::export(conversation, &segments, format, &options);
        let dir = dirs::download_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(std::env::temp_dir);
        let path = dir.join(format!(
            "{}.{}",
            sanitise_filename(&conversation.title),
            format.extension()
        ));

        match std::fs::write(&path, rendered) {
            Ok(()) => self.raise(format!("Exported to {}", path.display())),
            Err(err) => self.raise(format!("Could not write {}: {err}", path.display())),
        }
    }

    /// Starts or stops the voxtype user service, then re-reads its real state
    /// rather than assuming the action worked.
    fn set_service_running(&mut self, running: bool) {
        let outcome = if running {
            fc_voxtype::service::start()
        } else {
            fc_voxtype::service::stop()
        };
        if let Err(err) = outcome {
            let verb = if running { "start" } else { "stop" };
            self.raise(format!("Could not {verb} voxtype.service: {err}"));
        }
        self.voxtype_service = crate::env::service_status();
    }

    /// Re-reads the capture sources, keeping the user's choice selected if it
    /// is still there. An index into a list that has changed underneath is how
    /// a picker crashes, so the selection is matched by identity and only
    /// falls back to the first entry when the chosen source is really gone.
    fn refresh_sources(&mut self) {
        let chosen = self.sources.get(self.selected_source).cloned();
        let probe = crate::env::list_sources();
        if let Some(problem) = probe.problem {
            self.raise(problem);
        }
        self.sources = probe.value;
        self.selected_source = reselect(chosen.as_ref(), &self.sources);
    }

    fn raise(&mut self, message: impl Into<String>) {
        let message = message.into();
        tracing::warn!(%message, "raising a banner");
        self.banner = Some(Banner { message });
    }

    /// Re-reads the library after it changes on disk.
    fn reload_library(&mut self) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let library = crate::env::load_library(&store);
        if let Some(problem) = library.problem {
            self.raise(problem);
            return;
        }
        self.conversations = library.value.conversations;
        self.groups = library.value.groups;
        self.tags = library.value.tags;
        self.conversation_tags = library.value.conversation_tags;
    }

    fn elapsed(&self) -> Duration {
        self.accumulated
            + self
                .session_start
                .map(|start| start.elapsed())
                .unwrap_or_default()
    }

    fn open_history(&mut self, id: ConversationId) {
        self.main_view = MainView::History(id);
        if let (Some(store), false) = (self.store.as_ref(), self.history_segments.contains_key(&id))
        {
            let segments = crate::env::load_segments(store, id);
            self.history_segments.insert(id, segments);
        }
    }

    fn segments_for(&self, id: ConversationId) -> &[Segment] {
        self.history_segments
            .get(&id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    fn drain_events(&mut self, ctx: &egui::Context) {
        // Collected first so the match below can take `&mut self` freely.
        let mut batch = Vec::new();
        if let Some(events) = &self.events {
            while let Ok(event) = events.try_recv() {
                batch.push(event);
            }
        }
        for event in batch {
            match event {
                SessionEvent::Level { peak, rms } => {
                    if self.state == SessionState::Recording {
                        self.level_peak = peak;
                        self.level_rms = rms;
                    }
                }
                SessionEvent::Provisional(segment) => {
                    if self.state == SessionState::Recording {
                        self.provisional.insert(segment.track, segment);
                    }
                }
                SessionEvent::Committed(segment) => {
                    if matches!(
                        self.state,
                        SessionState::Recording | SessionState::Finishing
                    ) {
                        self.provisional.remove(&segment.track);
                        self.segments.push(segment);
                        overlay::sync(&self.overlay, &self.segments);
                    }
                }
                // Routed through `set_state` so the elapsed-time bookkeeping
                // happens for transitions the session reports, not only for
                // the ones a button starts.
                SessionEvent::StateChanged(state) => self.set_state(state),
                SessionEvent::PressureChanged(pressure) => self.pressure = pressure,
                SessionEvent::SourceLost { reason } => {
                    self.banner = Some(Banner {
                        message: format!("Audio source lost: {reason}"),
                    });
                }
                SessionEvent::SourceRecovered => self.banner = None,
                SessionEvent::Failed { stage, message } => {
                    self.banner = Some(Banner {
                        message: format!("{stage}: {message}"),
                    });
                }
            }
        }
        if matches!(
            self.state,
            SessionState::Recording | SessionState::Finishing
        ) {
            ctx.request_repaint_after(Duration::from_millis(60));
        }
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
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
                fastframe_tray::Event::Show => {
                    if headless {
                        self.wants_show = true;
                    } else {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    }
                }
                fastframe_tray::Event::Menu("quit") => {
                    self.quit_requested = true;
                    if !headless {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
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

    fn window_gone(&mut self) {
        self.overlay_open = false;
    }

    fn headless_frame(&mut self, ctx: &egui::Context) -> fastframe_shell::Headless {
        self.drain_tray(ctx, true);
        if self.quit_requested {
            fastframe_shell::Headless::Quit
        } else if std::mem::take(&mut self.wants_show) {
            fastframe_shell::Headless::Show
        } else {
            fastframe_shell::Headless::Wait
        }
    }

    fn shutdown(&mut self) {
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
fn source_combo(app: &mut App, ui: &mut egui::Ui, id_salt: &str) {
    let current = match app.sources.get(app.selected_source) {
        Some(source) => source.label(),
        None => t("No audio source").to_owned(),
    };
    egui::ComboBox::from_id_salt(id_salt)
        .selected_text(current)
        .show_ui(ui, |ui| {
            if app.sources.is_empty() {
                ui.label(t("Nothing to record"));
            }
            for index in 0..app.sources.len() {
                let label = app.sources[index].label();
                ui.selectable_value(&mut app.selected_source, index, label);
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

fn service_pill(ui: &mut egui::Ui, status: ServiceStatus) {
    let (icon, text, color) = match status {
        ServiceStatus::Running => (
            crate::icons::Icon::StatusOk,
            t("voxtype: running"),
            egui::Color32::from_rgb(0x4c, 0xaf, 0x50),
        ),
        ServiceStatus::Stopped => (
            crate::icons::Icon::StatusWarn,
            t("voxtype: stopped"),
            egui::Color32::from_rgb(0xe0, 0x6c, 0x75),
        ),
        ServiceStatus::Unknown => (
            crate::icons::Icon::StatusWarn,
            t("voxtype: unknown"),
            egui::Color32::GRAY,
        ),
    };
    ui.add(icon.image(color, 14.0));
    ui.colored_label(color, text);
}

fn format_elapsed(elapsed: Duration) -> String {
    let total_secs = elapsed.as_secs();
    format!(
        "{:02}:{:02}:{:02}",
        total_secs / 3600,
        (total_secs / 60) % 60,
        total_secs % 60
    )
}

/// A flat purple disc: fastcription has no app icon yet, so the tray draws
/// one procedurally rather than shipping a placeholder asset.
fn tray_icon_rgba(size: usize) -> Vec<u8> {
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

/// The name a conversation gets until the user renames it.
fn default_title() -> String {
    let now = time::OffsetDateTime::now_local()
        .unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    let format = time::macros::format_description!(
        "[year]-[month]-[day] [hour]:[minute]"
    );
    now.format(&format)
        .map(|stamp| format!("Conversation {stamp}"))
        .unwrap_or_else(|_| "Conversation".to_owned())
}

/// Finds the user's previously chosen source in a freshly enumerated list.
///
/// Matched on identity rather than position: a source that disappears shifts
/// every index after it, and quietly recording a different stream than the one
/// the user picked is the worst failure this app has. Falls back to the first
/// entry only when the previous choice is genuinely gone.
fn reselect(previous: Option<&AudioSource>, sources: &[AudioSource]) -> usize {
    previous
        .and_then(|previous| {
            sources.iter().position(|source| {
                source.kind == previous.kind
                    && source.name == previous.name
                    && source.application == previous.application
            })
        })
        .unwrap_or(0)
}

/// Keeps a user-chosen title usable as a filename without surprising them with
/// a mangled name: separators and control characters go, everything else stays.
fn sanitise_filename(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '\0') {
                '-'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').to_owned();
    if trimmed.is_empty() {
        "conversation".to_owned()
    } else {
        trimmed
    }
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}







#[cfg(test)]
mod tests {
    use super::{reselect, sanitise_filename};
    use fc_core::{AudioSource, SourceKind};

    fn monitor(name: &str) -> AudioSource {
        AudioSource::named(SourceKind::SinkMonitor, name, name)
    }

    #[test]
    fn a_refresh_keeps_the_chosen_source_when_the_list_shifts() {
        let previous = monitor("b.monitor");
        let after = vec![monitor("new.monitor"), monitor("a.monitor"), monitor("b.monitor")];
        assert_eq!(reselect(Some(&previous), &after), 2);
    }

    #[test]
    fn a_vanished_source_falls_back_to_the_first() {
        let previous = monitor("gone.monitor");
        let after = vec![monitor("a.monitor")];
        assert_eq!(reselect(Some(&previous), &after), 0);
    }

    #[test]
    fn an_empty_list_selects_index_zero_without_panicking() {
        let previous = monitor("a.monitor");
        assert_eq!(reselect(Some(&previous), &[]), 0);
        assert_eq!(reselect(None, &[]), 0);
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
        assert_eq!(reselect(Some(&firefox), &after), 1);
    }

    #[test]
    fn filenames_survive_a_title_with_separators() {
        assert_eq!(sanitise_filename("1:1 with Alice"), "1-1 with Alice");
        assert_eq!(sanitise_filename("a/b\\c"), "a-b-c");
        assert_eq!(sanitise_filename("   "), "conversation");
        assert_eq!(sanitise_filename("..."), "conversation");
    }
}
