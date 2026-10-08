//! Starting, pausing and stopping a recording, and draining what the running
//! session reports.
//!
//! Split out of `app/mod.rs` so the control path can be read on its own: this
//! is the only place that decides a session may begin, and the only place that
//! turns a [`SessionEvent`] into app state.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use fc_core::{EngineInfo, SessionEvent, SessionState};

use crate::app::{App, NoticeKind};
use crate::i18n::{t, tf};
use crate::session::{self, Session, SessionConfig};

impl App {
    /// Re-seeds engine, model and language from voxtype's own configuration.
    ///
    /// Those three are seeded from `~/.config/voxtype/config.toml` only on the
    /// very first launch; after that the saved settings win, which is right
    /// for a user who changed them here and wrong for one who changed them
    /// there. This is the way back without deleting the saved settings.
    pub(super) fn reset_engine_to_voxtype(&mut self) {
        let defaults = crate::env::voxtype_defaults();
        let blank = super::settings::State::default();
        self.settings.engine = defaults.engine.unwrap_or(blank.engine);
        self.settings.model = defaults.model.unwrap_or(blank.model);
        self.settings.language = defaults.language.unwrap_or(blank.language);
        let message = crate::i18n::tf(
            "Engine set to {} / {} / {} from voxtype's configuration.",
            &[
                self.settings.engine.as_str(),
                self.settings.model.as_str(),
                self.settings.language.as_str(),
            ],
        );
        self.notify(super::NoticeKind::Info, message);
    }

    pub(super) fn set_state(&mut self, new: SessionState) {
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
                self.state = new;
                self.clear_session();
                // The conversation row is closed by now, so the sidebar can
                // show it with its real duration and segment count.
                self.reload_library();
                return;
            }
            (_, SessionState::Idle) => self.clear_session(),
            _ => {}
        }
        self.state = new;
    }

    /// Everything that only means something while a session runs.
    ///
    /// `pressure` and the level meter used to be left where the session ended,
    /// so an idle app kept claiming transcription was behind and showed a
    /// frozen input level for a source nothing was reading.
    fn clear_session(&mut self) {
        self.session_start = None;
        self.accumulated = Duration::ZERO;
        self.level_peak = 0.0;
        self.level_rms = 0.0;
        self.pressure = fc_core::Pressure::Keeping;
        self.provisional.clear();
        // The cached transcript of the conversation that just closed is short
        // by whatever committed after it was cached, and exporting from it
        // produced a transcript missing its own ending.
        if let Some(id) = self.recording.take() {
            self.history_segments.remove(&id);
        }
        if let Some(path) = self.session_config.take() {
            crate::env::remove_session_config(&path);
        }
    }

    /// Starts a new conversation, or resumes the paused one.
    pub(super) fn start_or_resume(&mut self) {
        if let Some(session) = &self.session {
            session.resume();
            return;
        }

        let Some(store) = self.store.clone() else {
            self.notify(
                NoticeKind::Error,
                "Recording is disabled because the conversation library could not be opened.",
            );
            return;
        };
        if self.library_read_only {
            self.notify(
                NoticeKind::Error,
                crate::env::read_only_library(&self.library_path),
            );
            return;
        }
        let Some(chosen) = self
            .selected_source
            .and_then(|index| self.sources.get(index))
            .cloned()
        else {
            self.notify(NoticeKind::Warning, "Choose an audio source first.");
            return;
        };
        // Refused up front rather than per chunk: without voxtype the session
        // would record happily and fail every transcription, which looks like
        // the app is working when nothing is being understood.
        if self.voxtype.is_none() {
            self.notify(NoticeKind::Error, t(super::readiness::NO_VOXTYPE));
            return;
        }
        if self.settings.model.trim().is_empty() && self.models.is_empty() {
            self.notify(NoticeKind::Error, t(super::readiness::NO_MODEL));
            return;
        }

        // Re-resolved rather than trusted: a sink-input index goes stale when
        // the application that owned it restarts, and recording the wrong
        // stream is worse than refusing.
        let source = match fc_audio::resolve(&chosen) {
            Ok(source) => source,
            Err(err) => {
                self.notify(
                    NoticeKind::Error,
                    tf(
                        "{} is not available: {}",
                        &[&chosen.label(), &err.to_string()],
                    ),
                );
                return;
            }
        };

        // Written per session, under this session's own name: the transcriber
        // re-reads it on every pass, so a config shared with the connection
        // test could retarget a running meeting at a server being tried out.
        let started_at = super::now_millis();
        let voxtype_config =
            match crate::env::write_session_config(&self.engine_settings(), started_at) {
                Ok(path) => path,
                Err(err) => {
                    self.notify(
                        NoticeKind::Error,
                        tf(
                            "Could not write fastcription's voxtype settings, so transcription \
                             would be too slow to follow live: {}",
                            &[&err],
                        ),
                    );
                    return;
                }
            };

        let config = SessionConfig {
            title: super::default_title(),
            group: None,
            source,
            mic_source: self.mic_track.then(|| self.mic_source.clone()),
            stream: self.stream_config(),
            engine: self.chosen_engine(),
        };

        match Session::start(
            store,
            config,
            self.transcriber_factory(Some(voxtype_config.clone())),
            &self.captures,
            started_at,
        ) {
            Ok(session) => {
                self.segments.clear();
                self.provisional.clear();
                self.events = Some(session.events.clone());
                self.recording = Some(session.conversation);
                self.live_conversation = Some(session.conversation);
                self.session_config = Some(voxtype_config);
                self.session = Some(session);
                self.set_state(SessionState::Recording);
                // So the new row appears, marked as recording.
                self.reload_library();
            }
            Err(err) => {
                crate::env::remove_session_config(&voxtype_config);
                self.notify(
                    NoticeKind::Error,
                    tf("Could not start recording: {}", &[&err.to_string()]),
                );
            }
        }
    }

    pub(super) fn pause(&mut self) {
        if let Some(session) = &self.session {
            session.pause();
        }
    }

    /// Hands the session off to finish on its own thread. `Finishing` shows
    /// immediately; the session reports `Idle` once the backlog is transcribed
    /// and the conversation is closed, and the library is reloaded then.
    pub(super) fn stop(&mut self) {
        if let Some(session) = self.session.take() {
            self.set_state(SessionState::Finishing);
            session.stop_async(super::now_millis());
        }
    }

    /// What the conversation will be transcribed with, as the settings pane
    /// currently has it. Recorded with the conversation so an old transcript
    /// can be read in the light of how it was made.
    pub(super) fn chosen_engine(&self) -> EngineInfo {
        EngineInfo {
            engine: self.settings.engine.clone(),
            model: self.settings.model.clone(),
            language: self.settings.language.clone(),
            backend: self.engine.backend.clone(),
        }
    }

    /// Builds the per-track transcriber.
    ///
    /// Everything voxtype needs is in fastcription's own config file, so the
    /// only argument is `-c`. Decision D6 still holds: the user's
    /// `config.toml` is read for defaults and never written. Keeping one
    /// source of truth matters here, because splitting model and language
    /// across command-line flags while the optimisation realtime depends on
    /// lives in a file is how the two drift apart.
    pub(super) fn transcriber_factory(
        &self,
        config: Option<PathBuf>,
    ) -> session::TranscriberFactory {
        let binary = self.voxtype.clone();
        let engine = self.settings.engine.clone();
        // Passed explicitly even though the config file also carries it: the
        // adapter supplies a thread count of its own by default, and a command
        // line that disagrees with the config would silently win.
        let threads = self.settings.threads.max(1);
        Box::new(move |_track| {
            let mut cli = fc_asr::VoxtypeCli::new().with_threads(threads);
            if let Some(path) = &binary {
                cli = cli.with_binary(path.display().to_string());
            }
            if let Some(path) = &config {
                cli = cli.with_config(path.clone());
            }
            // The engine is the one knob with no equivalent in the config file
            // fastcription writes, which only configures whisper.
            if !engine.trim().is_empty() {
                cli = cli.with_engine(engine.clone());
            }
            Box::new(cli)
        })
    }

    /// How the stream is tuned, from the settings pane.
    pub(super) fn stream_config(&self) -> fc_asr::StreamConfig {
        fc_asr::StreamConfig {
            step: Duration::from_secs_f32(self.settings.refresh_secs.clamp(0.3, 5.0)),
            max_utterance: Duration::from_secs_f32(
                self.settings.max_utterance_secs.clamp(4.0, 22.0),
            ),
            ..Default::default()
        }
    }

    /// Where transcription should run, as the settings pane has it.
    pub(super) fn engine_settings(&self) -> crate::env::EngineSettings {
        crate::env::EngineSettings {
            model: self.settings.model.clone(),
            language: self.settings.language.clone(),
            threads: self.settings.threads.max(1),
            fast_mode: self.settings.fast_mode,
            remote: (self.settings.remote_enabled
                && !self.settings.remote_endpoint.trim().is_empty())
            .then(|| crate::env::RemoteEngine {
                endpoint: self.settings.remote_endpoint.clone(),
                model: self.settings.remote_model.clone(),
                api_key: self.settings.remote_api_key.clone(),
                timeout_secs: self.settings.remote_timeout_secs,
            }),
        }
    }

    /// Transcribes a moment of silence through the configured server, so a
    /// wrong address or a missing key is found now rather than during a
    /// meeting. Goes through voxtype rather than a bare HTTP request, so it
    /// exercises the real path: the endpoint, the multipart body and the token.
    ///
    /// On a thread, because the only interesting failure — an address nothing
    /// answers at — takes the configured timeout to discover, and running it
    /// inline froze the window for up to two minutes.
    pub(super) fn probe_transcription_server(&mut self) {
        let binary = self.voxtype.clone();
        let endpoint = self.settings.remote_endpoint.trim().to_owned();
        let settings = self.engine_settings();
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("fc-server-probe".into())
            .spawn(move || {
                let _ = tx.send(probe_server(binary, &endpoint, &settings));
            });
        match spawned {
            Ok(_) => {
                self.settings.remote_probe = None;
                self.settings.remote_probe_rx = Some(rx);
            }
            Err(err) => {
                self.settings.remote_probe =
                    Some(Err(format!("Could not run the connection test: {err}")));
            }
        }
    }

    /// Picks up the answer from [`App::probe_transcription_server`].
    pub(super) fn poll_remote_probe(&mut self) {
        let Some(rx) = &self.settings.remote_probe_rx else {
            return;
        };
        match rx.try_recv() {
            Ok(result) => {
                self.settings.remote_probe = Some(result);
                self.settings.remote_probe_rx = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.settings.remote_probe_rx = None;
                if self.settings.remote_probe.is_none() {
                    self.settings.remote_probe =
                        Some(Err("The connection test did not finish".to_owned()));
                }
            }
        }
    }

    /// Applies the answers from the startup probes, once they arrive.
    pub(super) fn poll_startup(&mut self) {
        let Some(rx) = &self.startup else {
            return;
        };
        let Ok(startup) = rx.try_recv() else {
            return;
        };
        self.startup = None;

        if let Some(problem) = startup.sources.problem {
            self.notify(NoticeKind::Warning, problem);
        }
        self.sources = startup.sources.value;
        self.engine = startup.engine;
        self.engines = startup.catalog.engines;
        self.models = startup.catalog.models;
        // The source the user picked last time, matched by identity against
        // what the server offers now.
        if let Some(previous) = self.pending_source.take() {
            self.selected_source = super::chrome::reselect(Some(&previous), &self.sources);
            if self.selected_source.is_none() {
                self.notify(
                    NoticeKind::Warning,
                    tf(
                        "{} is not available any more, so nothing is selected to record.",
                        &[&previous.label()],
                    ),
                );
            }
        } else if self.selected_source.is_none() {
            // A first launch, or one with nothing remembered: pressing Start
            // must not be refused on a machine with exactly one source. The
            // default is the choice a meeting needs, not the first thing the
            // sound server happened to list.
            self.selected_source =
                super::chrome::default_selection(&self.sources, startup.default_monitor.as_deref());
        }
    }

    pub(super) fn drain_events(&mut self, ctx: &egui::Context) {
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
                    }
                }
                // Routed through `set_state` so the elapsed-time bookkeeping
                // happens for transitions the session reports, not only for
                // the ones a button starts.
                SessionEvent::StateChanged(state) => self.set_state(state),
                SessionEvent::PressureChanged(pressure) => self.pressure = pressure,
                SessionEvent::SourceLost { reason } => {
                    let id = self
                        .notify_id(NoticeKind::Warning, tf("Audio source lost: {}", &[&reason]));
                    self.capture_notice = Some(id);
                }
                // Only the notice it pairs with: clearing the list would also
                // take away a transcription failure or a refused write, which
                // the capture coming back says nothing about.
                SessionEvent::SourceRecovered => {
                    if let Some(id) = self.capture_notice.take() {
                        self.notices.remove(id);
                    }
                    self.notify(NoticeKind::Info, t("The audio source is back."));
                }
                SessionEvent::Failed { stage, message } => {
                    self.notify(NoticeKind::Error, tf("{}: {}", &[stage, &message]));
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

    pub(super) fn elapsed(&self) -> Duration {
        self.accumulated
            + self
                .session_start
                .map(|start| start.elapsed())
                .unwrap_or_default()
    }
}

/// The body of the connection test, on its own thread.
///
/// The config goes in a temporary file that is deleted when this returns: the
/// live config belongs to whatever session is running, and a test that wrote
/// over it would change what the running transcriber does on its next pass.
fn probe_server(
    binary: Option<PathBuf>,
    endpoint: &str,
    settings: &crate::env::EngineSettings,
) -> Result<String, String> {
    let Some(binary) = binary else {
        return Err("voxtype was not found on PATH".to_owned());
    };
    if endpoint.is_empty() {
        return Err("Enter the server's address first".to_owned());
    }

    let config = crate::env::write_probe_config(settings)
        .map_err(|err| format!("Could not write the settings: {err}"))?;
    let probe = crate::env::write_probe_wav().map_err(|err| err.to_string())?;

    let started = Instant::now();
    let output = std::process::Command::new(&binary)
        .arg("-c")
        .arg(config.path())
        .arg("-q")
        .arg("transcribe")
        .arg(probe.path())
        .output()
        .map_err(|err| format!("Could not run voxtype: {err}"))?;

    if output.status.success() {
        Ok(format!("Reached the server in {:?}", started.elapsed()))
    } else {
        // voxtype puts the reason on stderr; the last line is the useful one.
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("no reason given")
            .trim()
            .to_owned();
        Err(reason)
    }
}

pub(super) fn format_elapsed(elapsed: Duration) -> String {
    let total_secs = elapsed.as_secs();
    format!(
        "{:02}:{:02}:{:02}",
        total_secs / 3600,
        (total_secs / 60) % 60,
        total_secs % 60
    )
}
