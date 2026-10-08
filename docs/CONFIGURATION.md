# fastcription — Configuration

Every setting fastcription has, every file it reads or writes, and what each
one costs. For how to use the features these settings belong to, see
[USER_GUIDE.md](USER_GUIDE.md).

## 1. The Settings pane

`Ctrl+,` opens it and puts the transcript back. The pane is in five tabs,
grouped by *when* a setting is touched rather than by what it configures:

| Tab | What is in it |
| --- | --- |
| **Audio** | What to transcribe, and whether to capture your own voice too |
| **Transcription** | The engine, model and language; speed; responsiveness |
| **Server** | Running the model on another computer |
| **Appearance** | Theme, audio visualiser, transcript size |
| **System** | The voxtype service, importing, where the library is |

Settings are listed below by tab. "Persists" means the value is written to
`app.ron` (section 2) and restored at the next launch.

### Audio source

| | |
| --- | --- |
| **Audio source** | The one source that gets transcribed. Devices, sink monitors and single application streams. |
| Default | Nothing selected. There is no fallback: a source that cannot be resolved leaves the picker empty and says so. |
| Persists | Yes — as a descriptor: kind, source name, description and application name, re-resolved by identity at the next launch. A sink-input index is stored too but never trusted on load, because it goes stale the moment the producing application restarts. |
| Takes effect | At the next **Start**. Disabled while a session runs. |

| | |
| --- | --- |
| **Capture my microphone as a second track** | Adds your own voice as a second track, labelled **You**. |
| Default | Off |
| Persists | Yes |
| Takes effect | At the next **Start**. Disabled while a session runs. |

| | |
| --- | --- |
| **Microphone** | Which capture device the second track uses. Real capture devices only, never monitors. |
| Default | `Default microphone`, which is PulseAudio's `@DEFAULT_SOURCE@` alias for whatever you have set as your input. |
| Persists | Yes |
| Takes effect | At the next **Start**. Disabled while the mic track is off or a session runs. |

### Transcription

This whole group is disabled while **Run the model on another computer** is on:
the server decides which model runs, and these knobs would not reach it.

| | |
| --- | --- |
| **Engine** | voxtype's engine family. A dropdown of the engines voxtype reports as compiled into this build; a free-text field when `voxtype info engines` could not be read. |
| Default | voxtype's own configured `engine`, falling back to `whisper` |
| Persists | Yes |
| Takes effect | Next conversation |

The engine is the one knob with no equivalent in the config file fastcription
writes, which configures whisper only, so it is passed on the command line as
`--engine`. The model and the language go through the config file alone.
Threads go both ways — the adapter has a default of its own, and a command line
that disagreed with the config would silently win.

| | |
| --- | --- |
| **Model** | The model name as voxtype knows it. A dropdown of the models voxtype reports as *installed*; free text when `voxtype info models` could not be read. |
| Default | voxtype's own configured `whisper.model`, falling back to `base.en` |
| Persists | Yes |
| Takes effect | Next conversation |

Chosen from a list rather than typed because a mistyped model name only fails
on the first pass, which during a meeting is the worst possible moment to find
out. voxtype accepts an unknown name, falls back to its own default and exits
successfully, so fastcription treats the warning it prints as an error rather
than record a model that never ran.

| | |
| --- | --- |
| **Language** | A language code such as `en` or `es`, a comma-separated list, or `auto`. |
| Default | voxtype's own configured `whisper.language`, falling back to `en` |
| Persists | Yes |
| Takes effect | Next conversation |

| | |
| --- | --- |
| **Fast mode** | voxtype's `context_window_optimization`. Two to nearly three times faster for the short passes realtime needs. |
| Default | On |
| Persists | Yes |
| Takes effect | Next conversation |

See section 6 for the measurements and the one thing to watch for.

| | |
| --- | --- |
| **inference threads** | CPU threads voxtype uses per pass. Range 1–32. |
| Default | `min(8, CPU cores)` |
| Persists | Yes |
| Takes effect | Next conversation |

Also section 6 for why 8.

### Transcription server

| | |
| --- | --- |
| **Run the model on another computer** | Sends each pass to an OpenAI-compatible transcription server instead of running the model here. |
| Default | Off |
| Persists | Yes |
| Takes effect | Next conversation |

It persists deliberately: a setting that silently reverted would run your next
meeting on the wrong machine.

| | |
| --- | --- |
| **Address** | The server's base URL, e.g. `http://desktop.lan:8080`. voxtype posts to `{address}/v1/audio/transcriptions`. |
| Default | Empty. Remote mode is only actually applied when this is non-empty. |
| Persists | Yes |
| Takes effect | Next conversation, and on the next **Test connection** |

An `http://` address that is not `localhost` or `127.0.0.1` draws a warning
under the fields: the audio crosses the network in the clear.

| | |
| --- | --- |
| **Model** (server) | The model name to ask that server for. |
| Default | `whisper-1` |
| Persists | Yes |
| Takes effect | Next conversation |

| | |
| --- | --- |
| **API key** | Optional bearer token, shown masked. |
| Default | Empty |
| Persists | **No.** See section 2. |
| Takes effect | Next conversation, and on the next **Test connection** |

| | |
| --- | --- |
| **timeout s** | How long voxtype waits for the server. Range 5–120 seconds. |
| Default | 30 |
| Persists | Yes |
| Takes effect | Next conversation |

| | |
| --- | --- |
| **Test connection** | Transcribes half a second of silence through the configured server, through the real path — endpoint, multipart body and token. |
| Result | Shown beside the button: "not tested" before you run it, "asking the server…" with a spinner while it runs, then the round-trip time or the reason it failed. |
| Persists | No. "Reached the server in 240ms" says nothing about the network the app was restarted onto. |

The test writes its config to a temporary file that is deleted when it
finishes, never to the running session's config — the live transcriber re-reads
its config on every pass, and a test that wrote over it would retarget a
meeting in progress at a server you were only trying out.

Two more lines appear under this group when they apply: **Acceleration:** with
the backend voxtype reports (e.g. `CPU (AVX2)`), and a note that "voxtype did
not report its engines or models; the fields above are free text."

### Responsiveness

| | |
| --- | --- |
| **seconds between passes** | How much new speech accumulates before the sentence in flight is re-transcribed. Range 0.5–3.0. |
| Default | 1.0 |
| Persists | Yes |
| Takes effect | Next conversation |

| | |
| --- | --- |
| **longest utterance in seconds** | How long one sentence may run before it is finalised without a pause. Range 8.0–22.0. |
| Default | 20.0 |
| Persists | Yes |
| Takes effect | Next conversation |

The pane says it plainly: "The sentence being spoken is re-transcribed this
often, and words appear once two passes agree on them. Shorter means words
sooner and more CPU. Changes apply to the next conversation."

Section 6 explains what the two numbers do to latency and load, and why the
ceiling is 22.

### Transcript

| | |
| --- | --- |
| **text size** | How large the transcript is drawn — live view, conversation history and compact mode alike. Range 14–40 points, in steps of 1. |
| Default | 22 |
| Persists | Yes |
| Takes effect | Immediately |

The Live pane carries the same slider, and `Ctrl+=` / `Ctrl+-` / `Ctrl+0` reach
it from anywhere. A size restored from disk that is not a finite number falls
back to 22 rather than reaching the font stack.

### Theme

| | |
| --- | --- |
| **Theme** | Where the palette comes from: **Neon** (the app's own look), **Follow the desktop**, **Dark**, **Light**, or any palette file in `~/.config/fastcription/themes/` by name. |
| Default | Neon |
| Persists | Yes |
| Takes effect | Immediately |

**Neon** is the look the app was designed to have — near-black ground, an
electric cyan accent, and a spectrum that runs cyan to magenta. It is the
default because a desktop theme chosen for a terminal rarely resembles it.
**Follow the desktop** is the reason `fastframe-theme` was chosen: fastcription
then changes with Omarchy as you switch themes, with nothing to keep in sync,
for anyone who would rather it blended in. The quieter fixed choices are for
when neither is right.

Whatever the source, any colour that carries words is lifted until it is
readable against the panel behind it (WCAG AA, AAA for the transcript itself).
A theme file named here that has since been deleted falls back to the
desktop's rather than to nothing.

### Compact mode

| | |
| --- | --- |
| **Give compact mode its own theme** | When ticked, a second theme picker appears and compact mode uses it. Unticked, it follows the theme above. |
| Default | Unticked |
| Persists | Yes |
| Takes effect | On entering or leaving compact mode |

The two modes are the same window, so only one palette is ever live;
switching modes re-resolves it. The setting exists because the two are looked
at in different circumstances — the full window is worked in, and the caption
strip sits over somebody else's video call for an hour, where a plainer or
darker palette can be much easier to read off.

| | |
| --- | --- |
| **visualiser height** | How tall the spectrum is drawn in the caption strip, 46–72 points. |
| Default | 52 |
| Persists | Yes |
| Takes effect | Immediately |

This is the height of the whole controls row, because the clock and the two
buttons sit *over* the spectrum rather than beside it — which is why the
floor is what those buttons need rather than what the bars need. The ceiling
leaves the captions 74 of the strip's 170 points, two lines at the default
transcript size.

A value outside the range, or one that is not a number at all, is clamped or
falls back to the default rather than reaching the layout — the settings file
is plain text, and a strip with no room for captions is the one thing compact
mode must never become.

### Audio visualiser

| | |
| --- | --- |
| **Style** | **Bars**, **Mirrored**, **Waveform**, **Ribbons**, **Ring** or **Off**. |
| Default | Bars |
| Persists | Yes |
| Takes effect | Immediately |

| | |
| --- | --- |
| **Show it in the full window too** | Whether the footer draws the spectrum behind itself. Unticked, it shows a percentage level meter on a plate instead. Compact mode always shows the visualiser. |
| Default | On |
| Persists | Yes |
| Takes effect | Immediately |

This is a real spectrum, not a shape drawn from the volume: the capture thread
runs a 512-point transform over the newest 32 ms of audio twenty times a
second and reports 24 log-spaced bands between 60 Hz and 8 kHz. That is why it
is worth looking at — a voice moves its energy around constantly, so the
display says *what* is being heard and not merely how loudly.

The colours are the theme's. The two-tone gradient is the palette's own accent
rotated around the colour wheel, so the visualiser belongs to whatever theme
is active rather than being the one thing on screen that ignores it; a
monochrome theme stays monochrome.

The preview in this tab is fed a synthetic spectrum, because with nothing
being recorded every style would otherwise look identical. It says so
underneath. It is the only place in the app that draws audio nobody made.

**The display auto-ranges.** The bands are an honest measurement and honest
measurements of a meeting are quiet: a monitor at a normal system volume puts
speech around -50 dBFS in any one band, which is a fifth of the way up the
bar. Drawn literally the visualiser is a row of stubs whatever is happening,
and the shape — the part worth looking at — is squashed into the bottom of
the strip. So the display tracks the loudest band it has seen in the last few
seconds and scales to that, the way a meter with no fixed scale does. A quiet
room and a loud one both fill the strip and the shape is the same either way.
The reference rises instantly and falls slowly, so one loud syllable does not
shrink everything else for a second afterwards, and silence stays silent —
the gain multiplies zero and gets zero.

**Cost.** The transform is a few thousand multiplies twenty times a second,
far less than the `pactl` read that delivered the samples, and the analyser
allocates nothing per block. Drawing stops entirely once the display has
decayed to silence, so an idle window is not held at 60 fps for a row of flat
bars. `Off` skips both.

### voxtype service

Not a setting: a status word (`not installed`, `running` or `stopped`) and
**Start** / **Stop** buttons, which run `systemctl --user start|stop
voxtype.service` and then re-read the real state. fastcription transcribes
without the daemon; the service is what provides voxtype's own push-to-talk
dictation.

### Import

Not a setting either: **Import past voxtype meetings**, with a spinner and an
`N imported so far` count while it runs. See
[USER_GUIDE.md](USER_GUIDE.md) section 12.

### Settings that are not in the pane

Some state persists without having a control of its own:

- the window's geometry and egui's own memory, which eframe stores alongside
  the settings under keys of its own
- the chosen source, the microphone and the mic-track toggle, listed above

And some state deliberately does not persist: the export format, options and
destination, the search box, the group and tag filters, the main pane you had
open, and which Settings tab you were on. Each starts fresh — an export destination suggested for one
conversation, left set, wrote the next transcript over the first one's file.

## 2. Files and directories

All paths assume the usual XDG defaults; `XDG_DATA_HOME` and `XDG_CONFIG_HOME`
are honoured where they are set.

### `~/.local/share/fastcription/library.db`

The conversation library: a SQLite database, in WAL mode, with foreign keys on.
You will see `library.db-wal` and `library.db-shm` beside it — that is normal.

It holds:

- **conversations** — title, group, start and end instants, status
  (`active`/`completed`/`interrupted`), the audio source as its parts, whether
  a microphone track was captured, the engine, model, language and acceleration
  backend used, and the voxtype meeting id on imported ones
- **segments** — one row per finished utterance: which track, a per-track
  sequence number, start and end offsets in milliseconds, the text, a speaker
  name when diarisation gave one, and a reserved translation slot
- **groups** and **tags** — names, unique ignoring case; tags may carry a
  colour, though nothing in the interface sets one
- **conversation_tags** — which tags are on which conversation
- **segments_fts** — a full-text index over segment text, kept in step by
  triggers, which is what makes searching by what was said possible

The schema version lives in SQLite's own `PRAGMA user_version`. Migrations are
append-only and run on open. A library written by a *newer* fastcription is
refused rather than opened, because writing rows against a schema this build
does not know could cost you a transcript.

Deleting a conversation deletes its segments and tag links with it. Deleting a
group leaves its conversations, ungrouped; deleting a tag leaves its
conversations, untagged.

**Read-only behaviour.** If the file or its directory will not accept writes,
fastcription opens it read-only rather than reporting a failure: every past
transcript is still readable, searchable and exportable, and only recording and
importing are disabled. (Read-only *storage* needs a second attempt, since
SQLite cannot read a WAL database without writing an index file beside it;
fastcription retries with SQLite's `immutable=1`, which read-only media
satisfies by definition.) On a read-only library a conversation left `active`
by a crash cannot be marked interrupted — that fix is itself a write — so it
stays listed as active.

### `~/.local/share/fastcription/app.ron`

Settings, written by eframe in RON on exit and about every half minute. It
holds exactly the `Persists: Yes` rows from section 1, under one key of
fastcription's own, plus eframe's record of the window geometry and egui's
memory under two keys of its own.

**The API key is never written here.** This file is plain text in your data
directory, and the key is a bearer token: it is excluded from what is saved, and
everything else in the blob still restores. The key reaches voxtype through the
per-session config below, which is created readable by you alone, and lives in
memory for the rest of the session.

A blob written by an older build, missing a field this one has, still restores
the fields it does carry.

Note the order of precedence at startup: the settings pane is first seeded from
voxtype's own `config.toml`, then whatever `app.ron` holds replaces it. So
voxtype's defaults only show through on a first run, or after this file is
deleted.

### `~/.local/share/fastcription/panic.log`

Where a panic is written, with the app name and version. Nothing else goes
here; ordinary logging goes to the terminal.

### `~/.config/fastcription/voxtype-<started_at>.toml`

The voxtype configuration fastcription writes for **one session**, where
`<started_at>` is that session's start time in milliseconds since the Unix
epoch.

It exists because voxtype's two most valuable settings for this app have no
command-line flag: `context_window_optimization`, which realtime depends on,
and remote mode. `voxtype -c <file>` accepts an arbitrary config, so
fastcription keeps its own and passes it explicitly.

Its first lines say what it is:

```toml
# Written by fastcription. Edits are overwritten.
#
# This is not your voxtype configuration. fastcription never modifies
# ~/.config/voxtype/config.toml; it reads it for defaults and passes
# this file to `voxtype -c` instead.

[whisper]
```

Running the model locally, the rest is `model`, `language`,
`context_window_optimization` and `threads`. Against a server it is instead
`mode = "remote"`, `remote_endpoint`, `remote_model`, `remote_api_key` (only
when one is set), `remote_timeout_secs` and `language` — with no local `model`
and no `context_window_optimization`, since neither means anything when the
work happens elsewhere.

Four things about this file:

- **It is created mode `0600`**, readable by you alone, from the moment it
  exists rather than tightened afterwards — so a bearer token in it is never
  briefly world-readable.
- **It is written atomically**, through a `.tmp` file in the same directory
  that is then renamed. The transcriber opens this file on every pass and must
  never see it half written.
- **It is removed when the session ends**, and again when the app shuts down.
- **Stale ones are swept at startup**, before anything else happens, because a
  file left behind by a process that did not exit cleanly may hold an API key.
  The sweep removes `voxtype-*.toml`, any `.tmp` beside them, and
  `voxtype.toml` — the single shared name earlier builds used, which nothing
  reads any more.

Why one file per session rather than one shared file: the running transcriber
re-reads its config on every pass, so testing a new server address in the
middle of a meeting silently retargeted the live transcript at it.

### `~/.config/voxtype/config.toml`

**Read for defaults, never written.** This is ARCHITECTURE.md decision D6:

> **Never rewrite `~/.config/voxtype/config.toml`; keep our own config and pass
> `voxtype -c`.** The user's file is read for defaults and never modified —
> they keep ownership of their dictation setup.

fastcription reads four fields from it at startup to seed the settings pane, so
that leaving the pane alone reproduces what voxtype would have done by itself:
`engine`, `whisper.model`, `whisper.language` and `audio.device`. Every field
is optional and unknown keys are expected, not errors.

### `~/.config/fastcription/themes/`

Where fastcription looks for theme files. It otherwise follows the desktop
theme (Omarchy templates and built-in presets), and reloads when that changes.
Any theme from outside is held to minimum contrast ratios before it is applied.

### The export destination

No fixed path. Each export suggests your downloads directory (falling back to
your home directory, then to the system temporary directory), with the
conversation's title as the filename and the format's extension. It is an
editable field, a leading `~/` is expanded, and the suggestion is not
remembered between conversations.

## 3. What is read from voxtype, and when

fastcription never writes anything voxtype owns — not its config, not its
runtime trigger files, not its database.

| What | Command or path | When |
| --- | --- | --- |
| Configured defaults | `~/.config/voxtype/config.toml` | At startup, to seed the settings pane |
| The binary | `PATH`, then `/usr/bin/voxtype` | At startup |
| Version | `voxtype --version` | At startup, on the probe thread |
| Engines | `voxtype info engines` | At startup. Only engines marked compiled reach the dropdown. |
| Models | `voxtype info models` | At startup. Only models marked installed reach the dropdown. |
| Current engine state | `voxtype status --format json --extended` | At startup, for the model and acceleration backend recorded with each conversation |
| Service state | `systemctl --user show -p ActiveState,SubState,UnitFileState voxtype.service` | At startup and on a poll (see below) |
| Daemon runtime state | `$XDG_RUNTIME_DIR/voxtype/state` | Watched with inotify, for the service pill |
| Past meetings | `voxtype meeting list --limit <all>` and `voxtype meeting export <id> --format json` | Only when you press **Import past voxtype meetings** |
| Transcription | `voxtype -q -c <session config> [--engine E] --threads N transcribe <wav>` | Once per pass, about once a second per track |

The startup probes run on their own thread and the window opens before they
answer — together they cost about half a second, and waiting for them was half
a second of blank screen at every launch. Only the cheap probes run: version,
engines and models are about 10 ms for the three, while `voxtype info devices`
alone costs 240 ms and nothing here needs it.

The service pill combines two signals, because neither alone is enough.
`systemctl` knows whether the unit is active but says nothing until asked; the
daemon's runtime state file changes the instant it does something but does not
exist while the daemon is stopped. So an inotify watch turns any daemon
activity into an immediate re-check, and a slow poll catches the unit being
started or stopped from outside fastcription — every 15 seconds with the watch
working, every 3 without. A daemon started by hand counts as running even when
`systemctl` reports the unit absent.

Every probe fails on its own and says so. A missing model does not hide a found
binary; a failed `voxtype info` does not stop transcription, it only turns the
engine and model dropdowns into text fields.

## 4. Environment

| Variable | What fastcription does with it |
| --- | --- |
| `RUST_LOG` | Sets the log filter, in `tracing` syntax, e.g. `RUST_LOG=debug` or `RUST_LOG=fc_asr=debug,info`. Defaults to `info`. Logs go to the terminal. |
| `PATH` | Searched for `voxtype`; `pactl` and `parec` are run by name. |
| `XDG_RUNTIME_DIR` | Where voxtype's daemon state files are watched, under `$XDG_RUNTIME_DIR/voxtype/`. Without it, the service pill falls back to `systemctl` alone. |
| `XDG_DATA_HOME` | Where `library.db`, `app.ron` and `panic.log` go, under `fastcription/`. Defaults to `~/.local/share`. |
| `XDG_CONFIG_HOME` | Where the per-session voxtype config and the themes directory go, under `fastcription/`, and where `voxtype/config.toml` is read from. Defaults to `~/.config`. |
| `HOME` | Expanding a leading `~/` in an export path, and the fallbacks above. |

The single-instance guard registers the name `dev.fastcription.App`; a second
launch hands a "show" request to the first and exits.

Nothing else is read, and nothing is configured by environment variable — every
setting is in the pane.

## 5. The compositor rule and the desktop entry

**Compact mode** asks the compositor to keep its window on top, which on
Wayland is a hint rather than a guarantee, so it needs one window rule to float
above a fullscreened meeting window. The rule matches on the window title
`fastcription — captions` (with a U+2014 em dash and an ordinary space either
side), because that is what changes when compact mode is entered and reverts
when it is left; the app id stays `fastcription` for both shapes.
[OVERLAY.md](OVERLAY.md) has the rule for Hyprland, sway and river, and what
you get without it.

**The desktop entry** is `packaging/fastcription.desktop`; copy it to
`~/.local/share/applications/`. It runs `fastcription` from `PATH`, sets
`StartupWMClass=fastcription` so the launcher matches the window, and names an
icon `fastcription`. fastcription ships no icon file, so your theme falls back
to a generic one; the tray draws its own.

## 6. Transcription tuning, explained

### What a pass is

fastcription does not cut the audio into pieces and transcribe each once. It
accumulates the sentence being spoken, and every so often re-transcribes *that
whole sentence from its start*. Two consecutive passes are compared, and the
words they agree on from the beginning are published and never revised. The
rest — the tail — is shown in italics and replaced on the next pass.

Why re-doing the same audio is not wasteful: whisper always pads its input to
30 seconds internally, so a 2-second clip costs almost exactly what an
11-second clip costs (0.71 s against 0.78 s, measured). Transcribing a growing
sentence over and over is close to free compared with transcribing it once.

A sentence is finished by 0.4 seconds of silence, or by the **longest
utterance** ceiling, whichever comes first. Then it becomes one line in the
transcript and one row in the library.

Two numbers with no control of their own: silence is anything below an RMS of
0.01, measured in 20-millisecond windows, and a burst of speech shorter than
0.4 seconds is treated as noise and dropped rather than finalised. That second
one is why a cough does not become a line — left to itself, whisper hears a
two-tenths-of-a-second cough as the word "you", reliably.

### seconds between passes (default 1.0, range 0.5–3.0)

How much new *speech* accumulates before the next pass. Speech, not audio: a
step's worth of trailing silence has nothing new to transcribe, and running a
pass on it is how a two-tenths-of-a-second cough reaches the model at all.

Lower means words on screen sooner and more CPU. Higher means fewer passes,
less CPU, and a longer wait. At the default, words appear about 1.5–2 seconds
behind the speaker: measured at 100% of 66 words correct across a 30.4-second
six-utterance sample, at 23% of one CPU's time.

### longest utterance in seconds (default 20.0, range 8.0–22.0)

The ceiling at which a sentence is finalised even if nobody has paused. It
matters because of **Fast mode**: voxtype's context-window optimisation only
applies under 22.5 seconds, so the ceiling is kept below that and every
sentence benefits.

### Fast mode (default on)

This is voxtype's `context_window_optimization`, which has no command-line flag
— the only way to reach it is the config file fastcription writes, which is
why that file exists. Measured on an Intel Core Ultra 9 285K with `base.en` on
CPU:

| Window | Off | On | Speedup |
| --- | --- | --- | --- |
| 2 s | 0.71 s | 0.25 s | 2.80× |
| 5 s | 0.72 s | 0.27 s | 2.69× |
| 7 s | 0.75 s | 0.28 s | 2.64× |
| 11 s | 0.78 s | 0.40 s | 1.97× |

It is the single biggest lever on whether the transcript keeps up, and it is on
by default for that reason. **The one thing to watch for: turn it off if you
see words repeating.** It has no effect at all when a server does the work.

### inference threads (default min(8, cores), range 1–32)

whisper.cpp stops getting faster past about eight threads for the small English
models, and oversubscription costs time. Measured on a 24-core machine, a
7-second window with Fast mode on:

| Threads | Time | Share of one CPU at 1 s steps |
| --- | --- | --- |
| auto (24) | 0.28 s | 28% |
| **8** | **0.21 s** | **21%** |
| 4 | 0.28 s | 28% |
| 2 | 0.42 s | 42% |

Hence the default of `min(8, cores)` — the floor at your core count keeps a
smaller machine from being told to use more threads than it has. Even two
threads fits, which is what makes this usable on a laptop with no GPU. The
setting is ignored when a server does the work.

### The adaptive interval

A pass is triggered by accumulated audio, so on a machine where a pass takes
longer than the interval, a fixed interval never catches up and the backlog
grows without bound. fastcription therefore stretches the interval to at least
the duration of the last pass, plus a fifth for headroom, and shows
"transcription behind — words arrive late" while that is happening. It costs
nothing, because every pass transcribes the whole sentence anyway: fewer passes
is less repeated work, not less transcript. Measured on the same 30.4-second
sample with passes deliberately slowed past the interval, a fixed 1-second
interval ran at 149% duty and never caught up, while the adaptive interval ran
at 86%, used a third fewer passes and was *more* accurate. Captured audio waits
in memory rather than being dropped — 16 kHz mono costs 64 KB per second of
lag, which is cheap next to losing part of a meeting.


## Returning to voxtype's engine defaults

**Use voxtype's defaults**, a small button under the engine controls, re-reads the engine, model and language from `~/.config/voxtype/config.toml` without touching anything else that is saved.