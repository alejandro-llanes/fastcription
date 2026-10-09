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
| D3 | **One window, with a compact mode.** The main window shrinks to a borderless, always-on-top caption bar showing the last four committed lines and the one being spoken, and restores on its own **Restore** button, Escape or Ctrl+M. | The first design used a second always-on-top window, an egui *deferred viewport*. Review found it had never worked: the app's repaint requests reach only the root viewport, so after its first frame the overlay never repainted; it synced only on finished utterances, a sentence behind the speaker; and hiding the main window to the tray destroyed it, because a deferred viewport is a child of the root — so the one arrangement a user wants, main window out of the way and captions over the call, was the one that could not exist. Making the main window *be* the caption bar needs no second viewport, repaints correctly for free, and still works with a compositor rule, now matched on the window title. The trade is that hiding to the tray hides the captions too, which is what hiding means. |
| D4 | **English transcript now; a second text slot per segment from day one.** | Translation is wired in once the realtime path is proven, without a schema migration. voxtype's `--translate` only goes *into* English, so this is ours to build. The interface shows no control for it: advertising a feature that does nothing is worse than its absence, so the slot and the column exist and nothing in the UI mentions them. |
| D5 | **Our own SQLite library.** | voxtype's `index.db` has no groups and no tags, and fastcription does not write into another application's database. Past voxtype meetings are imported read-only. |
| D6 | **Never rewrite `~/.config/voxtype/config.toml`; keep our own config and pass `voxtype -c`.** | The user's file is read for defaults and never modified — they keep ownership of their dictation setup. But voxtype's most valuable knob for this app, `context_window_optimization`, has no command-line flag, and realtime depends on it. `voxtype -c <file>` accepts an arbitrary config, so fastcription writes one of its own and passes it explicitly. **One per session**, at `~/.config/fastcription/voxtype-<started_at>.toml`: the running transcriber re-reads its config on every pass, so a single shared file meant that testing a server address mid-meeting silently retargeted the live transcript at it. Written atomically through a `.tmp` sibling and created `0600`, since it can hold a bearer token; removed when the session ends, and stale ones (including the old shared `voxtype.toml`) are swept at startup. The file says in a comment that it is ours and gets overwritten. |
| D7 | **Segments are persisted as they are committed.** | A crash mid-meeting costs one chunk, not the meeting. This is a deliberate improvement over voxtype's save-on-stop. |
| D10 | **Transcription may run on another machine, through voxtype's remote mode.** | The model stays resident on a box with a GPU, so a laptop with none gets a large model's accuracy and pays no per-pass load. It needs no new code in the transcription path: voxtype's remote mode speaks the OpenAI audio API, and fastcription already writes the config that selects it. Verified against upstream's `remote.rs` and exercised end to end against a mock server in the test suite. The cost is that audio leaves the machine, which the settings pane warns about, and that each pass uploads the utterance so far — about 32 KB per second of speech. See `docs/SERVER.md`. |
| D9 | **Re-transcribe the current utterance about once a second and commit words once two consecutive passes agree** (LocalAgreement-2), instead of transcribing disjoint chunks once each. | Chunking meant reading a sentence roughly eight seconds after it was spoken — chunk length plus inference — which is useless for following a live conversation, and it was the first thing testing exposed. Because Whisper pads to 30 s anyway, re-transcribing a growing utterance costs little more than transcribing it once, and the optimisation above pays for the repetition. Measured: 100% of 66 words correct across a 30.4 s six-utterance sample at 23% of one CPU's time, with words appearing ~1.5–2 s behind the speaker. Every pass sees the utterance from its start, so there are no chunk boundaries to lose context across and no overlap to reconcile — this deleted the segmenter and the dedup pass outright. The trade is that a committed word is never revised, so an occasional word commits early and wrong. |
| D11 | **Settings and the chosen source persist across launches; the API key does not.** | A user who configured a transcription server must not re-enter it every time, and `remote_enabled` silently reverting would run their next meeting on the wrong machine. eframe's storage is a plain-text file, so the key is excluded from it; it already lives user-only in the per-session voxtype config and is kept for the session. |
| D12 | **The visualiser draws a real spectrum, computed on the capture thread.** | An animation driven from the peak level can only breathe in and out: every sound looks the same, and it says nothing the level meter did not. A spectrum says *what* is being heard, which is what makes it worth the screen space — it answers "is this still hearing the call?" from across a room, where a percentage answers it only to someone close enough to read a number. A 512-point transform over the newest 32 ms, twenty times a second, costs far less than the `pactl` read that delivered the samples, and the analyser allocates nothing per block. It rides on `SessionEvent::Level`, which already flows at the right rate, rather than on a second channel to keep in step. See `fc_audio::spectrum`. |
| D13 | **The palette may be pinned instead of following the desktop, and the visualiser's colours are derived from it.** | Following Omarchy is the reason `fastframe-theme` was chosen and remains one pick away; the default is now the app's own look (D16). But compact mode sits over a call for an hour, and a theme that is pleasant to work in is not always the right thing to read captions off, so the choice is offered. The visualiser then cannot have colours of its own without being the one thing on screen that ignores the theme: its two-tone gradient is the palette's own accent rotated around the colour wheel, which keeps the user's hue and still separates the low and high ends. A monochrome theme stays monochrome. |
| D14 | **One visual language, in `ui.rs`, rather than widgets chosen per call site.** | "A heading" had been `ui.heading` in one pane, a bold `RichText` in another and a plain label in a third; a button that mattered looked exactly like one that did not; and a row of controls disagreed about its own height. The window read as a pile of widgets. `ui.rs` fixes the vocabulary: three depths (window, bar, panel) spent only on grouping, one control height, quiet upper-case labels over bright values, and a halo reserved for the thing that is currently true. Every colour still comes from the palette, so the language survives whatever theme the desktop is wearing. The cost is a layer of indirection between a pane and egui, which is the point: a pane can no longer invent a shape. |
| D15 | **The footer draws the spectrum behind itself, and everything on it that carries words sits on an opaque chip.** | A translucent bar over a live spectrum is the look that was asked for, and it cannot be had honestly by putting labels straight onto it: the colour behind a label then depends on how loud that band happens to be, and the arithmetic said the bar had to be 96% opaque for the dimmest of them to keep its 3:1 — which is the same as not drawing a spectrum. Plating the labels instead moves the question to a background the palette already guarantees (`window`), so the scrim is free to be light. The same change fixed a defect in the chips themselves, whose fill had been a tint of their own text colour and so measured 2.95:1 for the faintest of them. |
| D16 | **The app ships its own look, `Palette::neon`, and opens in it.** | The first pass kept following the desktop and kept every effect restrained, and the result was a tasteful muted dark UI — not the thing that was asked for, which was an audio plugin's black ground, one saturated hue that reads as light, and a spectrum that owns the bottom of the window. A desktop theme chosen for a terminal was never going to produce that. So the look is designed and shipped: near-black depths, cyan accent, a 120° spread to magenta across the spectrum, glows that reach fourteen points, and one big round button per bar. Every colour that carries words is still held to its contrast floor by the same test as the other palettes, and following the desktop is still a setting. |
| D17 | **`parec` runs at 50 ms latency.** | Left at the sound server's default it handed audio over in two-second chunks — measured: the capture loop saw nothing for 2 s and then forty 50 ms level windows at once. Two things followed. The visualiser held one spectrum for two seconds and jumped, which reads as the display freezing; and the transcriber received every word up to two seconds after it was said, which is a large share of the lag live captions carry for no reason at all. At 50 ms — one level window — events arrive one per frame (median 43 ms, max 86 ms). The gain envelope was tuned at the same time: with its reference rising instantly, the loudest band sat pinned at the target height whatever the room was doing, so the display could change shape but never size. |
| D18 | **A registry of words and expressions, with meanings from a language model on the user's own server.** | The transcript gets a non-native listener through the meeting; what they are still missing afterwards is the handful of expressions in it they had never met, and a dictionary has "table" and "ocean" and nothing for "table this" or "boil the ocean". A small instruction-tuned model, asked about the expression *in the line it was said in*, answers that — measured at 0.4 s on the user's RTX 5070 beside the whisper model, with `gemma3:4b` the only one of three candidates whose Spanish could be trusted (SERVER.md). It runs on Ollama on the same box as the transcription server, with the same privacy posture: the user's own network, plain HTTP. The entry always shows the English meaning beside the translation and is editable, because the model's translation of an idiom is sometimes literal and the meaning is what catches it. The words are selected in the transcript itself, by drag or double-click: egui's own label selection keeps the selected text private, so the transcript lays out and paints its rows by hand and owns the selection (`select.rs`). A lookup asks the vocabulary before the server, so a word already paid for — and perhaps corrected — is never asked again. Schema v2; `ureq` without TLS as the one new dependency, since the app had no HTTP client and the server is never across the internet. |
| D8 | **No async runtime in the app core.** | std threads plus `crossbeam-channel`, waking egui with `ctx.request_repaint()`. egui is a synchronous immediate-mode loop; a tokio runtime would buy nothing and complicate the audio path. |

---

## 3. Process and thread topology

```
                    ┌──────────────── UI thread (eframe/egui) ────────────────┐
                    │  live view · history · compact mode · settings · export │
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
          │  remainder = unstable tail, shown italic, replaced       │
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
transcript does not flicker. The tail of the current pass is drawn in italics,
in the palette's `secondary`, and replaced on each pass — that is where a word
that has not settled yet lives. Italics rather than `dim`, which measured 3:1
against the panel on a light palette and made the newest words on screen the
hardest ones to read. On finalise the whole utterance becomes one committed row
and one line in the transcript.

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
  main.rs            eframe bootstrap; fastframe wiring (log, instance, shell,
                     tray); the single-instance "show" request
  env.rs             what the app reads from and writes to the machine: the
                     library, capture sources, voxtype's catalog and service
                     state (probed on a thread so the window opens at once),
                     the per-session voxtype config, import of past meetings
  session.rs         the supervisor: capture threads, one TranscriptStream per
                     track, the store handle; the only place that knows how
                     the crates fit together
  theme.rs           the palette, following the Omarchy desktop theme
  i18n.rs            the seam every user-facing string passes through
  icons.rs           the Lucide icons the chrome draws, and nothing else
  app/
    mod.rs           the App struct, notices (a short levelled log), what is
                     persisted across launches (settings and the chosen
                     source; never the API key)
    chrome.rs        attach, frame, top bar, status, compact mode, shortcuts,
                     the tray Resident, source re-selection by identity
    session_control.rs  start / pause / stop, draining session events,
                     engine settings, the threaded server probe
    library.rs       conversations, groups, tags, search, export, import
    live.rs          the live transcript
    readiness.rs     the four things recording needs, and their remedies
    transcript.rs    one transcript row, shared by the live and history views,
                     and the clipboard text that goes with it
    sidebar.rs       groups, tags, search box, conversation list
    history.rs       one conversation: rename, group, tags, transcript
    settings.rs      source, microphone, engine, responsiveness, server,
                     service, import
    export_ui.rs     format picker, options and destination
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

Implementations: `VoxtypeCli`, and only that one. A `VoxtypeMeeting` that
delegated to meeting mode was removed along with the rest of the meeting-mode
wrappers (§4) — it could not produce live text, and shipping unreachable code
is a liability. `VoxtypeLive` arrives if §8 lands upstream.

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
application)` — the application name taken from `application.name`, falling
back to `application.process.binary` and then `media.name` — and re-resolved at
session start; if it cannot be resolved the UI asks rather than silently
falling back. The picker's own re-selection matches on `(kind, name,
application)`; resolving a sink input matches on application *and* description
first, since `media.name` is what tells two streams of one application apart,
and a tier matching several streams is reported as ambiguous rather than
resolved by picking one. Device removal mid-session triggers reconnect with a
banner, and the session keeps its transcript.

> **This machine, right now:** PipeWire exposes only `auto_null` /
> `auto_null.monitor` despite two HDA cards being present in `/proc/asound/cards`.
> Nothing is recordable until wireplumber/device access is fixed. Worth resolving
> before the first end-to-end test, or capture work will look broken when it isn't.

---

## 6. Storage

`~/.local/share/fastcription/library.db`, rusqlite, WAL, versioned migrations.

```sql
conversations(
  id INTEGER PRIMARY KEY, title TEXT NOT NULL,
  group_id INTEGER REFERENCES groups(id) ON DELETE SET NULL,
  started_at INTEGER NOT NULL, ended_at INTEGER, status TEXT NOT NULL,
  source_kind TEXT NOT NULL, source_name TEXT NOT NULL, source_desc TEXT NOT NULL,
  source_application TEXT, source_index INTEGER,
  mic_track INTEGER NOT NULL,
  engine TEXT NOT NULL, model TEXT NOT NULL, language TEXT NOT NULL, backend TEXT,
  voxtype_meeting_id TEXT    -- set only on imported meetings
)
segments(
  id INTEGER PRIMARY KEY, conversation_id INTEGER NOT NULL REFERENCES conversations(id)
    ON DELETE CASCADE,
  seq INTEGER NOT NULL, track TEXT NOT NULL,
  start_ms INTEGER NOT NULL, end_ms INTEGER NOT NULL,
  text TEXT NOT NULL, translation TEXT, speaker TEXT, confidence REAL
)
groups(id INTEGER PRIMARY KEY, name TEXT NOT NULL COLLATE NOCASE UNIQUE,
       created_at INTEGER NOT NULL)
tags(id INTEGER PRIMARY KEY, name TEXT NOT NULL COLLATE NOCASE UNIQUE, color TEXT)
conversation_tags(conversation_id REFERENCES conversations(id) ON DELETE CASCADE,
                  tag_id REFERENCES tags(id) ON DELETE CASCADE,
                  PRIMARY KEY(conversation_id, tag_id))
segments_fts  -- FTS5 external-content over segments.text, kept in step by triggers
-- UNIQUE(conversation_id, track, seq): an utterance is appended exactly once,
-- so a retried append after an ambiguous commit fails loudly instead of
-- silently duplicating a line in the user's transcript.
```

The schema version is SQLite's own `PRAGMA user_version`, not a table. Steps
are append-only and each runs in its own transaction, so a crash mid-migration
cannot leave the version ahead of what was applied; a version this build does
not know is refused rather than opened.

Committed segments are inserted as they arrive (D7). Groups are flat for now; the
`group_id` column is nullable so nesting can be added later without touching
`segments`.

A library whose file or directory refuses writes is opened read-only rather
than reported as a failure: every past transcript stays readable, searchable
and exportable, and only recording and importing are disabled. Read-only
*storage* needs a second attempt with SQLite's `immutable=1`, because reading a
WAL database otherwise means writing a wal-index beside it.

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
  on the runtime `state` file. The watch acts only on events a write produces:
  `notify` subscribes to `IN_OPEN`, so the app's own reads of that file come back
  to it as events, and answering each with another read made the watcher its own
  event source — a core spun and the update queue grew by 13 MB/s for as long as
  the app ran, idle or not (v0.1.0). Note that the live path does *not* need the daemon —
  service control exists for the user's dictation workflow and for meeting-mode
  delegation, as `CLAUDE.md` requires.
- **The level meter is our own**, peak and RMS computed on the capture thread
  twenty times a second from the audio already being captured. Reusing
  `voxtype-audio-bridge` was considered and not done: it reports what the
  *daemon* hears, which on this design is a different stream from the one
  fastcription is transcribing, and it needs the daemon running when the live
  path deliberately does not. Nothing in the app reads `audio.sock`.

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

Workspace-wide, by the crate that uses it:

```toml
rusqlite    = { version = "0.40", features = ["bundled"] }   # fc-store
hound       = "3"      # fc-asr: the WAV each pass hands to voxtype
notify      = "8"      # fc-voxtype: inotify on the voxtype runtime dir
toml        = "0.9"    # fc-voxtype read, fastcription write
time        = "0.3"    # every instant; not chrono
crossbeam-channel = "0.5"
serde / serde_json / anyhow / thiserror / tracing / tracing-subscriber
dirs / tempfile
```

The binary, on top of those:

```toml
eframe      = { version = "0.36.1", features = ["glow", "wayland", "x11", "persistence"] }
egui        = "0.36.1"   # both via the fastframe-pinned fork
egui_extras = { version = "0.36.1", features = ["svg"] }

fastframe-log      = { git = "https://github.com/crmne/fastframe", rev = "bb79dbd" }
fastframe-instance = { git = "…", rev = "bb79dbd" }
fastframe-fonts    = { git = "…", rev = "bb79dbd" }
fastframe-text     = { git = "…", rev = "bb79dbd" }
fastframe-scroll   = { git = "…", rev = "bb79dbd" }
fastframe-icons    = { git = "…", rev = "bb79dbd" }
fastframe-theme    = { git = "…", rev = "bb79dbd" }
fastframe-shell    = { git = "…", rev = "bb79dbd" }
fastframe-tray     = { git = "…", rev = "bb79dbd" }
```

`fastframe-i18n` is deliberately absent: it compiles PO catalogs at build time
and expects `.po` files per locale, which is real localisation work with
nothing to translate yet. `src/i18n.rs` is the seam for it — every user-facing
string already passes through one function — not a replacement for it. So is
`fastframe-update`: there is no release channel to check against.

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
| Chunk boundaries cut words | No longer reachable: D9 deleted the chunks. Every pass transcribes the utterance from its start, so there is no boundary to lose context across. The residual risk is the opposite one — a word committed early and never revised — which the settled/unsettled display at least makes visible as it happens |
| Always-on-top on Wayland | winit gives no layer-shell, so compact mode asks for `WindowLevel::AlwaysOnTop` and relies on a compositor rule matched on the compact title (`docs/OVERLAY.md` has Hyprland, sway and river). Without the rule it is an ordinary window the user keeps in front by hand. |
| Transcription falls behind on a slow machine or a large model | Unbounded PCM channel on which `push` blocks, so audio waits instead of being dropped; the pass interval stretches to at least the last pass's duration; "transcription behind — words arrive late" in the status bar; `base.en` and `context_window_optimization` on by default; the model can move to another machine (D10) |
| fastframe API churn ("early, APIs will change") | Pinned revisions; fastframe crates are confined to `main.rs` and the view layer |
| No usable PipeWire devices on the dev machine | Fix wireplumber before the first capture test (§5) |
| Sink-input indices are unstable | Persist a resolvable descriptor, re-resolve at start, ask rather than guess |
