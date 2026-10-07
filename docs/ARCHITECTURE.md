# fastcription — Architecture

Realtime conversation transcription for Linux desktops. A frontend for
[voxtype](https://github.com/peteonrails/voxtype), built in Rust on
[fastframe](https://fastframe.dev) (egui).

The point of the app: someone attending a meeting in a language they read better
than they hear can follow the conversation as text, live, while it happens.

> Naming: `CLAUDE.md` says "fastscription", the repository is `fastcription`.
> This document uses **fastcription** throughout. Settle the spelling before the
> first release — it leaks into the binary name, the desktop file, the config
> path and the data directory.

---

## 1. What voxtype gives us

Verified against voxtype 1.0.1 (`voxtype-bin`) and upstream `main`.

| Surface | Detail |
| --- | --- |
| `voxtype transcribe <file>` | 16 kHz mono WAV in, transcript on stdout. No daemon required. Honors `--engine --model --language --threads --translate`. |
| `voxtype meeting …` | `start/stop/pause/resume/status/list/show/export/label/summarize/delete`. Export: `text`, `markdown`, `json`, `srt`, `vtt`. |
| Meeting control IPC | Trigger files in `$XDG_RUNTIME_DIR/voxtype/`: `meeting_start` (optional title), `meeting_stop`, `meeting_pause`, `meeting_resume`, `meeting_start_diarization`. The CLI only writes these files; the daemon polls them. |
| Daemon state | `$XDG_RUNTIME_DIR/voxtype/state` → `idle` \| `recording` \| `transcribing`. `meeting_state` → two lines, `status\nmeeting_id`. Also `pid`, `version`, `voxtype.lock`. |
| Status stream | `voxtype status --format json --extended --follow` emits a JSON line per state change (model, device, backend). |
| Audio levels | `voxtype-audio-bridge` reads `$XDG_RUNTIME_DIR/voxtype/audio.sock` and emits NDJSON: `{"peak":0.42,"rms":0.18,"vad":1,"ts_ms":…}`, plus `{"status":"connected"|"disconnected"}`. |
| Meeting storage | `~/.local/share/voxtype/meetings/index.db` (SQLite: `meetings`, `speaker_labels`) and one directory per meeting with `metadata.json` + `transcript.json`. |
| Device naming | `[audio] device` and `[meeting.audio] mic_device` are **ALSA/cpal** names (`default`, `pipewire`, `hw:…`). `[meeting.audio] loopback_device` is a **PulseAudio/PipeWire source name** — it shells out to `parec --device <name>`. Mind the asymmetry. |
| Engines | whisper, parakeet, moonshine, sensevoice, paraformer, dolphin, omnilingual, cohere, soniox. Parakeet has true streaming knobs (`streaming_chunk_secs`, `streaming_left_context_secs`, `streaming_right_context_secs`). |

### The gap that shapes everything

In meeting mode the transcript lives in daemon memory. `MeetingEvent::ChunkProcessed`
is consumed by a single `tracing::debug!` call, and `save_transcript()` runs only
when the meeting stops. **voxtype has no live transcript feed.** Driving
`voxtype meeting` cannot satisfy the realtime requirement as shipped.

Second gap: meeting mode always opens the microphone through cpal, so "transcribe
only the source I selected" is not expressible there either.

### Measured cost of the chosen path

`voxtype -q transcribe` on an Intel Core Ultra 9 285K, `base.en`, CPU (AVX2):

| Input | Wall time | Notes |
| --- | --- | --- |
| 6.93 s of speech | **0.78 s** | includes process spawn + model load; transcript verbatim correct |
| model load alone | 0.04 s | ggml file is mmapped, warm page cache |

~9× realtime. Per-chunk process spawn is not a problem. Steady-state lag is
`chunk_length + ~0.8 s`.

Output contract: with `-q`, stdout is three banner lines (`Loading audio file:`,
`Audio format:`, `Processing N samples`), a blank line, then the transcript.
whisper.cpp's own C-level logging goes to stderr. This is not a versioned
contract — it lives behind one adapter with a test (§7).

---

## 2. Decisions

| # | Decision | Rationale |
| --- | --- | --- |
| D1 | **Own the capture, use `voxtype transcribe` per chunk.** Ship this; pursue a live feed upstream in parallel. | Works against the stock package today, puts latency and source selection fully under our control. Measured at 0.78 s per 7 s chunk, so the per-chunk spawn cost that worried us is a non-issue. |
| D2 | **One selected source by default; the microphone is an opt-in second track.** | `CLAUDE.md` asks for the selected source only. The mic toggle, labelled `You` vs `Remote`, is what makes a transcript a record of a conversation rather than half of one. |
| D3 | **Main window plus a separate always-on-top caption overlay.** | The meeting window has to stay visible. The overlay shows the last few lines; the main window owns control, history, tagging and export. |
| D4 | **English transcript now; a second text slot per segment from day one.** | Translation is wired in once the realtime path is proven, without a schema migration. voxtype's `--translate` only goes *into* English, so this is ours to build. |
| D5 | **Our own SQLite library.** | voxtype's `index.db` has no groups and no tags, and fastcription does not write into another application's database. Past voxtype meetings are imported read-only. |
| D6 | **Never rewrite `~/.config/voxtype/config.toml`.** | Engine, model and language go through per-invocation CLI flags. voxtype's config is read for defaults only. Users keep ownership of their dictation setup. |
| D7 | **Segments are persisted as they are committed.** | A crash mid-meeting costs one chunk, not the meeting. This is a deliberate improvement over voxtype's save-on-stop. |
| D8 | **No async runtime in the app core.** | std threads plus `crossbeam-channel`, waking egui with `ctx.request_repaint()`. egui is a synchronous immediate-mode loop; a tokio runtime would buy nothing and complicate the audio path. |

---

## 3. Process and thread topology

```
                    ┌──────────────── UI thread (eframe/egui) ────────────────┐
                    │  live view · history · overlay · settings · export      │
                    └───▲───────────────────────────────┬─────────────────────┘
      Event channel     │                               │  Command channel
   (segments, levels,   │                               │  (start/stop/pause,
    state, errors)      │                               │   source change)
                    ┌───┴───────────────────────────────▼─────────────────────┐
                    │                   Session supervisor                    │
                    └───┬───────────────┬───────────────┬─────────────────────┘
                        │               │               │
          ┌─────────────▼──┐   ┌────────▼───────┐  ┌────▼──────────────┐
          │ capture thread │   │ capture thread │  │   store thread    │
          │   (selected)   │   │  (mic, opt.)   │  │ rusqlite, WAL     │
          └─────────┬──────┘   └────────┬───────┘  └────▲──────────────┘
                    │  PCM + peak/RMS            │      │ committed segments
          ┌─────────▼──────────────────────────┐ │      │
          │ segmenter (per track)              │ │      │
          │  ring buffer · RMS/VAD gate        │ │      │
          │  silence-boundary cuts             │ │      │
          │  min 1.5s / target 7s / max 15s    │ │      │
          │  0.5s overlap                      │ │      │
          └─────────┬──────────────────────────┘ │      │
                    │ Chunk { track, seq, pcm, t0_ms, provisional }           │
          ┌─────────▼───────────────────────────────────────────┐             │
          │ ASR worker (one per track, ordered, bounded queue)  ├─────────────┘
          │  tmp WAV → `voxtype -q transcribe` → Vec<Segment>   │
          └─────────────────────────────────────────────────────┘
```

**Capture.** `parec --device <source> --raw --format=s16le --rate=16000 --channels=1`,
the same mechanism voxtype uses for its loopback track. A subprocess here is a
feature: it is already how voxtype reaches monitor sources, it survives a
PipeWire restart, and it costs us no `libpipewire` binding. Behind a
`CaptureBackend` trait so a native `pipewire-rs` backend can replace it.

**Backpressure.** The ASR queue is bounded at 2. On overflow the segmenter grows
its target chunk length (7 s → 10 s → 15 s) instead of dropping audio, and the UI
shows a "transcription behind" indicator. Audio is never discarded silently.

**Two-tier results.** A short 1.5–2 s chunk yields a *provisional* segment,
rendered dimmed; the overlapping 7 s chunk replaces it with a *committed*
segment, which has more context and so transcribes better. Only committed
segments reach SQLite. This is what makes the app feel live without overstating
its accuracy. Overlap means the tail of one chunk repeats in the head of the
next: committed segments are joined with a word-level tail/head dedup (voxtype
solves the analogous problem with `dedup_bleed_through`).

---

## 4. Module layout

A cargo workspace of six library crates plus the binary.

> **Deviation from the first sketch.** This document originally called for one
> binary crate with modules, splitting "only when something is genuinely
> reused". The split came early for a different reason: each concern compiles
> and tests independently, so work on the store cannot be blocked by a
> half-written capture path, and `cargo test -p fc-asr` does not build egui.
> The cost is a handful of extra manifests and an explicit dependency graph,
> which is a fair price.

```
crates/
  fc-core/       domain types every other crate speaks in: AudioSource, Segment,
                 Track, Conversation, Group, Tag, EngineInfo, SessionEvent.
                 No I/O, no toolkit, nothing that could pull one in.
  fc-store/      SQLite library: migrations, conversations, segments, groups,
                 tags, FTS5 search. Segments persisted as they commit.
  fc-audio/      source enumeration (pactl -f json) and capture (parec),
                 behind a CaptureBackend trait; peak/RMS metering.
  fc-asr/        segmenter (VAD, chunk boundaries, overlap, backpressure),
                 Transcriber trait, the `voxtype transcribe` adapter, dedup.
  fc-voxtype/    the only crate that knows voxtype exists: CLI probes, read-only
                 config parse, systemd unit control, runtime-state watcher,
                 meeting-mode wrappers.
  fc-export/     txt / md / json / srt / vtt writers.
  fastcription/  the binary: eframe + fastframe, the session supervisor that
                 joins capture to ASR to store, and the egui views.
```

The dependency graph is a DAG with `fc-core` at the bottom and nothing but the
binary depending on more than one sibling.

Inside the binary crate:


```
src/
  main.rs            eframe bootstrap; fastframe wiring (log, theme, fonts,
                     text, icons, instance, tray, shell, update)
  session.rs         the supervisor: owns the capture threads, the segmenter,
                     the ASR workers and the store handle; the only place that
                     knows how the crates fit together
  app/
    mod.rs           App state, drains SessionEvent each frame, repaint plumbing
    live.rs          live transcript: auto-scroll, sticky bottom, segment rows
    overlay.rs       always-on-top caption window (last N lines)
    sidebar.rs       groups tree, tag filter, conversation list, search box
    history.rs       conversation detail, rename, regroup, retag, speaker labels
    settings.rs      source, engine/model, chunking, service control, translation
    export_ui.rs     format picker and destination
```

### Transcriber trait

```rust
pub struct Segment {
    pub track: Track,              // Selected | Microphone
    pub seq: u64,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    pub translation: Option<String>,  // D4: slot exists, unused for now
    pub speaker: Option<String>,
    pub confidence: Option<f32>,
    pub provisional: bool,
}

pub trait Transcriber: Send {
    fn transcribe(&self, pcm: &[f32], sample_rate: u32) -> Result<Vec<Segment>>;
    fn describe(&self) -> EngineInfo;   // engine, model, backend — for the UI and the record
}
```

Implementations: `VoxtypeCli` (ships), `VoxtypeMeeting` (delegation, no live text),
`VoxtypeLive` (once §8 lands upstream).

---

## 5. Audio source selection

`CLAUDE.md`'s requirement is that only the chosen source is transcribed, so source
selection is a first-class part of the UI, not a settings afterthought.

Enumerated from `pactl -f json list`:

- **sources** — real capture devices and monitors, with `description` for display.
- **sinks** — each offered as its `<sink>.monitor`, which is how you capture
  "everything the system is playing".
- **sink-inputs** — individual application streams. `parec --monitor-stream=<index>`
  captures exactly one application's output, so a user can transcribe the Zoom
  call without picking up their music. This is the most valuable option in the
  list and the reason we enumerate sink inputs at all.

Handling: a sink-input index is not stable across restarts of the producing
application, so the chosen source is persisted as `(kind, name, description,
app-binary)` and re-resolved at session start; if it cannot be resolved the UI
asks rather than silently falling back. Device removal mid-session triggers
reconnect with a banner, and the session keeps its transcript.

> **This machine, right now:** PipeWire exposes only `auto_null` /
> `auto_null.monitor` despite two HDA cards being present in `/proc/asound/cards`.
> Nothing is recordable until wireplumber/device access is fixed. Worth resolving
> before the first end-to-end test, or capture work will look broken when it isn't.

---

## 6. Storage

`~/.local/share/fastcription/library.db`, rusqlite, WAL, versioned migrations.

```sql
conversations(
  id INTEGER PRIMARY KEY, title TEXT, group_id INTEGER REFERENCES groups(id),
  started_at INTEGER NOT NULL, ended_at INTEGER, status TEXT NOT NULL,
  source_kind TEXT, source_name TEXT, source_desc TEXT,
  engine TEXT, model TEXT, language TEXT,
  voxtype_meeting_id TEXT    -- set only on imported meetings
)
segments(
  id INTEGER PRIMARY KEY, conversation_id INTEGER NOT NULL REFERENCES conversations(id)
    ON DELETE CASCADE,
  seq INTEGER NOT NULL, track TEXT NOT NULL, start_ms INTEGER, end_ms INTEGER,
  text TEXT NOT NULL, translation TEXT, speaker TEXT, confidence REAL
)
groups(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, created_at INTEGER)
tags(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, color TEXT)
conversation_tags(conversation_id, tag_id, PRIMARY KEY(conversation_id, tag_id))
segments_fts  -- FTS5 external-content over segments.text, for search
schema_version(version INTEGER NOT NULL)
```

Committed segments are inserted as they arrive (D7). Groups are flat for now; the
`group_id` column is nullable so nesting can be added later without touching
`segments`.

**Export** formats match voxtype's set — `txt`, `md`, `json`, `srt`, `vtt` — so a
fastcription transcript and a voxtype transcript are interchangeable downstream.
Each exporter takes the same `(Conversation, Vec<Segment>, ExportOptions)`, where
options cover timestamps, speaker labels and a metadata header, mirroring
`voxtype meeting export`.

---

## 7. voxtype integration rules

- **Shell out to the CLI, not to the trigger files.** `voxtype meeting start` and
  friends only write files into `$XDG_RUNTIME_DIR/voxtype/`, and we could write
  them ourselves, but the CLI is the contract that upstream maintains.
- **One adapter owns stdout parsing**, with a golden test over recorded output
  (including the `Loading audio file:` / `Audio format:` / `Processing …` banner
  and the degenerate-transcript retry path, which emits `WARN` lines and an empty
  result). voxtype's CLI output is not a versioned interface; treat a parse
  failure as a reportable error, never as an empty transcript.
- **Check the binary and version at startup** (`voxtype --version`, `voxtype info
  engines|models`) and tell the user precisely what is missing: no binary, no
  model downloaded, engine not compiled in.
- **Service control**: `systemctl --user start|stop|restart voxtype.service`;
  state from `systemctl --user show -p ActiveState,SubState` plus an inotify watch
  on the runtime `state` file. Note that the live path does *not* need the daemon —
  service control exists for the user's dictation workflow and for meeting-mode
  delegation, as `CLAUDE.md` requires.
- **Reuse `voxtype-audio-bridge`** for the level meter when the daemon is running;
  fall back to our own capture-thread peak/RMS otherwise.

---

## 8. Upstream work (D1, second half)

Two patches to voxtype would let fastcription drop its own chunker and become a
pure frontend. Both are small and useful to every voxtype consumer:

1. **A live transcript feed.** Either append each chunk's segments to
   `transcript.jsonl` in the meeting directory as they are produced, or emit them
   on a Unix socket alongside the existing `audio.sock`. `MeetingEvent::ChunkProcessed`
   already carries exactly the right payload; today it only reaches a `tracing::debug!`.
   The JSONL variant is the smaller patch and also makes meeting mode crash-safe.
2. **`[meeting.audio] mic_device = "disabled"`**, symmetric with the existing
   `loopback_device = "disabled"`, so a meeting can capture one chosen source only.

With both merged, `VoxtypeLive` becomes the default `Transcriber` and the
segmenter is retired. Until then `VoxtypeCli` is the shipping path, and nothing
about the UI, the store or the export layer depends on which one is active.

---

## 9. Dependencies

```toml
[dependencies]
eframe      = "…"   # egui + winit, via the fastframe-pinned fork
egui        = "…"
rusqlite    = { version = "…", features = ["bundled", "functions"] }
crossbeam-channel = "…"
notify      = "…"   # inotify on the voxtype runtime dir
serde / serde_json
hound       = "…"   # WAV out for chunk handoff
anyhow / thiserror
chrono

fastframe-theme  = { git = "https://github.com/crmne/fastframe", rev = "bb79dbd" }
fastframe-fonts  = { git = "…", rev = "bb79dbd" }
fastframe-text   = { git = "…", rev = "bb79dbd" }
fastframe-icons  = { git = "…", rev = "bb79dbd" }
fastframe-log    = { git = "…", rev = "bb79dbd" }
fastframe-tray   = { git = "…", rev = "bb79dbd" }
fastframe-shell  = { git = "…", rev = "bb79dbd" }
fastframe-instance = { git = "…", rev = "bb79dbd" }
fastframe-scroll = { git = "…", rev = "bb79dbd" }
fastframe-i18n   = { git = "…", rev = "bb79dbd" }
fastframe-update = { git = "…", rev = "bb79dbd" }
```

Nothing in fastframe is published on crates.io yet — pin a revision, not a
branch, and move it deliberately. The app's root `Cargo.toml` must also carry the
`[patch.crates-io]` block from fastframe's README pinning the `crmne/egui`
(`ba6790fe`) and `crmne/winit` (`ed7caa90`) forks; patch every egui crate the app
uses from one revision, and move egui and winit together. On stock egui/winit the
app still builds, without those fixes.

`fastframe-theme` follows the Omarchy desktop theme with filesystem
notifications, which on this machine means fastcription matches the rest of the
desktop for free.

---

## 10. Risks

| Risk | Mitigation |
| --- | --- |
| voxtype CLI output changes | One adapter, golden test, version check at startup, parse failure is an error |
| Chunk boundaries cut words | 0.5 s overlap + word-level tail/head dedup; provisional/committed two-tier display |
| Always-on-top overlay on Wayland | winit gives no layer-shell; ship a Hyprland window rule and document the equivalent for sway/river. Fall back to a normal window. |
| Transcription falls behind on a slow machine or a large model | Bounded queue, growing chunk length, visible indicator; recommend `base.en` or Parakeet; never drop audio |
| fastframe API churn ("early, APIs will change") | Pinned revisions; fastframe crates are confined to `main.rs` and the view layer |
| No usable PipeWire devices on the dev machine | Fix wireplumber before the first capture test (§5) |
| Sink-input indices are unstable | Persist a resolvable descriptor, re-resolve at start, ask rather than guess |
