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
| `voxtype meeting …` | `start/stop/pause/resume/status/list/show/export/label/summarize/delete`. Export: `text`, `markdown`, `json` **only**. Upstream's `MEETING_MODE.md` and README advertise SRT and VTT, but the 1.0.1 binary's `--help` does not offer them — trust the binary. fastcription writes SRT and VTT itself. |
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
| 11 s of speech (JFK sample) | **0.78 s** | verbatim correct |
| 2 s of speech | **0.71 s** | **the same cost as 11 s** |
| model load alone | 0.04 s | ggml file is mmapped, warm page cache |

**Whisper always pads its input to 30 seconds**, which is why a 2 second clip
costs what an 11 second clip costs. Short clips buy nothing, and a design that
transcribes many small pieces pays full price for each.

`context_window_optimization = true` changes that for clips under 22.5 seconds:

| Window | Default | With the optimisation | Speedup |
| --- | --- | --- | --- |
| 2 s | 0.71 s | 0.25 s | 2.80× |
| 5 s | 0.72 s | 0.27 s | 2.69× |
| 7 s | 0.75 s | 0.28 s | 2.64× |
| 11 s | 0.78 s | 0.40 s | 1.97× |

It has no command-line flag, only a config key — see D6 on how fastcription
reaches it without touching the user's configuration.

Thread scaling, 7 s window with the optimisation on, on 24 cores:

| Threads | Time | Duty cycle at 1 s steps |
| --- | --- | --- |
| auto (24) | 0.28 s | 28% |
| **8** | **0.21 s** | **21%** |
| 4 | 0.28 s | 28% |
| 2 | 0.42 s | 42% |

whisper.cpp stops scaling past about eight threads for the small English
models, and oversubscription costs time, so the default is `min(8, cores)`.
Even two threads fits, which is what makes this viable on a laptop with no GPU.

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
| D4 | **English transcript now; a second text slot per segment from day one.** | Translation is wired in once the realtime path is proven, without a schema migration. voxtype's `--translate` only goes *into* English, so this is ours to build. The interface shows no control for it: advertising a feature that does nothing is worse than its absence, so the slot and the column exist and nothing in the UI mentions them. |
| D5 | **Our own SQLite library.** | voxtype's `index.db` has no groups and no tags, and fastcription does not write into another application's database. Past voxtype meetings are imported read-only. |
| D6 | **Never rewrite `~/.config/voxtype/config.toml`; keep our own config and pass `voxtype -c`.** | The user's file is read for defaults and never modified — they keep ownership of their dictation setup. But voxtype's most valuable knob for this app, `context_window_optimization`, has no command-line flag, and realtime depends on it. `voxtype -c <file>` accepts an arbitrary config, so fastcription writes `~/.config/fastcription/voxtype.toml` and passes it explicitly. That file says in a comment that it is ours and gets overwritten. |
| D7 | **Segments are persisted as they are committed.** | A crash mid-meeting costs one chunk, not the meeting. This is a deliberate improvement over voxtype's save-on-stop. |
| D10 | **Transcription may run on another machine, through voxtype's remote mode.** | The model stays resident on a box with a GPU, so a laptop with none gets a large model's accuracy and pays no per-pass load. It needs no new code in the transcription path: voxtype's remote mode speaks the OpenAI audio API, and fastcription already writes the config that selects it. Verified against upstream's `remote.rs` and exercised end to end against a mock server in the test suite. The cost is that audio leaves the machine, which the settings pane warns about, and that each pass uploads the utterance so far — about 32 KB per second of speech. See `docs/SERVER.md`. |
| D9 | **Re-transcribe the current utterance about once a second and commit words once two consecutive passes agree** (LocalAgreement-2), instead of transcribing disjoint chunks once each. | Chunking meant reading a sentence roughly eight seconds after it was spoken — chunk length plus inference — which is useless for following a live conversation, and it was the first thing testing exposed. Because Whisper pads to 30 s anyway, re-transcribing a growing utterance costs little more than transcribing it once, and the optimisation above pays for the repetition. Measured: 100% of 66 words correct across a 30.4 s six-utterance sample at 23% of one CPU's time, with words appearing ~1.5–2 s behind the speaker. Every pass sees the utterance from its start, so there are no chunk boundaries to lose context across and no overlap to reconcile — this deleted the segmenter and the dedup pass outright. The trade is that a committed word is never revised, so an occasional word commits early and wrong. |
| D8 | **No async runtime in the app core.** | std threads plus `crossbeam-channel`, waking egui with `ctx.request_repaint()`. egui is a synchronous immediate-mode loop; a tokio runtime would buy nothing and complicate the audio path. |

---

## 3. Process and thread topology

```
                    ┌──────────────── UI thread (eframe/egui) ────────────────┐
                    │  live view · history · overlay · settings · export      │
                    └───▲───────────────────────────────┬─────────────────────┘
      Event channel     │                               │  start / pause / stop
   (stable text, the    │                               ▼
    unstable tail,  ┌───┴───────────────────────────────────────────────────────┐
    levels, state)  │                   Session supervisor                      │
                    └───┬───────────────────────────────┬───────────────────────┘
                        │                               │
          ┌─────────────▼──────────┐         ┌──────────▼─────────────┐
          │ capture (selected src) │         │ capture (mic, optional)│
          │   parec → PCM frames   │         │                        │
          └─────────────┬──────────┘         └──────────┬─────────────┘
                        │  unbounded: audio waits here, never dropped
          ┌─────────────▼───────────────────────────────▼─────────────┐
          │ stream thread, one per track (fc_asr::TranscriptStream)   │
          │                                                          │
          │  accumulate utterance ──► every ~1 s ──► transcribe the  │
          │  (VAD; silence before            whole utterance from    │
          │   speech discarded)              its start               │
          │                                        │                 │
          │  longest common prefix of the last two passes = stable   │
          │  remainder = unstable tail, shown dimmed, replaced       │
          │                                                          │
          │  silence ≥ 0.4 s, or 20 s elapsed ──► finalise utterance │
          └─────────────┬────────────────────────────────────────────┘
                        │ finalised utterance
                 ┌──────▼──────────┐
                 │ store (rusqlite)│  one row per utterance, on finalise
                 └─────────────────┘
```

**Capture.** `parec --device <source>` or `--monitor-stream=<index>`, the same
mechanism voxtype uses for its own loopback track. A subprocess here is a
feature: it is already how voxtype reaches monitor sources, it survives a
PipeWire restart, and it costs us no `libpipewire` binding. Behind a
`CaptureHandle` trait, so a native `pipewire-rs` or `pw-record` backend can
replace it without touching the supervisor — and so the whole pipeline can be
driven by a fake capture in tests, which on a machine with no working audio
device is the difference between the supervisor being tested and not.

**Why audio is never dropped.** The PCM channel is unbounded and `push` blocks
for the length of one transcription pass. When transcription falls behind,
captured audio waits in the channel instead of being discarded — 16 kHz mono
f32 costs 64 KB per second of lag, which is cheap next to losing part of a
meeting.

**Why the interval adapts.** A pass is triggered by accumulated *audio*, so on a
machine where a pass takes longer than the interval, a fixed interval never
catches up: the backlog grows without bound. Stretching the interval to at least
the duration of the last pass fixes it, and costs nothing, because every pass
transcribes the whole utterance anyway — fewer passes means less repeated work,
not less transcript. Measured on the same 30.4 s sample with a transcriber slowed
until each pass took longer than the interval:

| | Duty cycle | Passes | Longest interval | Accuracy |
| --- | --- | --- | --- | --- |
| Fixed 1 s interval | **149%** (never catches up) | 26 | 1.0 s | 98% |
| Adaptive interval | **86%** (keeps up) | 15 | 2.1 s | **100%** |

This is load-bearing, not a refinement.

**End to end, through the real voxtype binary**, six sentences separated by
silence, fed from a separate thread paced on a wall clock so the audio timeline
never stalls:

| | |
| --- | --- |
| Utterances finalised | 6 of 6, boundaries on the silences |
| Latency from end of utterance to committed text | 0.37 s – 0.77 s, flat across the session |
| Duty cycle | 38.6% of one CPU's time |

Measuring this needs care. Feeding frames synchronously from the same thread
that transcribes stalls the audio clock whenever a pass overruns its frame
budget, and the reported latency then grows steadily — indistinguishable from a
pipeline that cannot keep up. The first version of this test measured its own
drift and read as 1.9 s climbing to 8.7 s. Pace the producer independently.

**What the user sees.** Stable words are append-only and never revised, so the
transcript does not flicker. The tail of the current pass is shown dimmed and
replaced on each pass, which is where a word that has not settled yet lives. On
finalise the whole utterance becomes one committed row and one line in the
transcript.

**What this replaced.** A segmenter that cut disjoint chunks on silence, a
bounded chunk queue with a growth ladder for backpressure, a separate shorter
"provisional" inference, and a word-level overlap dedup pass. None of it is
needed when every pass covers the utterance from its start.

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
  fc-asr/        the streaming transcriber: utterance accumulation with VAD,
                 re-transcription on an interval, LocalAgreement-2 stability,
                 the Transcriber trait and the `voxtype transcribe` adapter.
  fc-voxtype/    the only crate that knows voxtype exists: CLI probes, read-only
                 config parse, systemd unit control, runtime-state watcher, and
                 the two meeting-mode calls import needs (list and export).
                 The wrappers for driving meeting mode were removed: nothing
                 delegates to it, and shipping unreachable code is a liability.
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
  env.rs             what the app reads from the machine it runs on: the
                     library, the capture sources, voxtype's configured engine
                     and its service state. Each probe fails on its own, since
                     the useful states are partial — a missing voxtype is no
                     reason to hide a past transcript
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

**Export** covers `txt`, `md`, `json`, `srt` and `vtt`. The first three match
what `voxtype meeting export` produces, so those transcripts are interchangeable
downstream; the subtitle formats are fastcription's own, since the 1.0.1 binary
does not offer them whatever its documentation says. Each exporter takes the same
`(Conversation, Vec<Segment>, ExportOptions)`, where the options cover timestamps,
speaker labels and a metadata header, mirroring voxtype's flags.

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
