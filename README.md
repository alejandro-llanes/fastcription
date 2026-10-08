# fastcription

Realtime conversation transcription for the Linux desktop. A graphical frontend
for [voxtype](https://github.com/peteonrails/voxtype), written in Rust on
[fastframe](https://fastframe.dev) (egui).

It exists for one job: if you follow a language better read than heard, you can
sit in a meeting and read what is being said, as it is said.

## What it does

- **Transcribes one source, the one you chose.** A capture device, the monitor
  of an output sink, or a single application's audio stream — so you can
  transcribe the call without transcribing your music.
- **Shows the transcript live**, about one and a half to two seconds behind the
  speaker, with the words that have settled kept apart from the tail still
  being revised.
- **Shrinks to a caption strip**, borderless and always on top, to sit over the
  meeting window.
- **Optionally captures your microphone too**, as a second track, so the
  transcript records both sides of the conversation.
- **Keeps a library**: conversations with custom names, grouped and tagged,
  searchable by what was actually said — not just by title — and exported to
  text, Markdown, JSON, SRT or WebVTT.
- **Imports past voxtype meetings** recorded by voxtype's own meeting mode.
- **Starts and stops the voxtype systemd user service** from the UI, for your
  dictation setup — fastcription's own transcription needs no daemon.

Everything runs locally by default. Audio does not leave the machine unless you
point it at a transcription server yourself — see
[docs/SERVER.md](docs/SERVER.md), which is how a desktop with a GPU can do the
work for a laptop without one.

## Requirements

- Linux with PipeWire or PulseAudio (`pactl` and `parec` on `PATH`)
- [voxtype](https://github.com/peteonrails/voxtype) with at least one model
  installed (`voxtype setup --download`)
- Rust 1.98 or newer to build

## Build

```sh
cargo run -p fastcription
```

The first build compiles egui and winit from the forks fastframe pins, which
takes a while.

For a desktop entry, install the release binary onto `PATH` and copy
`packaging/fastcription.desktop` into `~/.local/share/applications/`.

The always-on-top caption strip needs one window rule on Wayland, because
winit cannot ask the compositor to keep a window above others.
[docs/OVERLAY.md](docs/OVERLAY.md) has it for Hyprland, sway and river.

## Shortcuts

| Key | |
| --- | --- |
| `Ctrl+R` | Start, or resume a paused conversation |
| `Ctrl+Space` | Pause / resume |
| `Ctrl+.` | Stop |
| `Ctrl+M` | Compact mode on or off (`Esc` comes back) |
| `Ctrl+=` / `Ctrl+-` / `Ctrl+0` | Transcript text larger, smaller, back to 22 pt |

None of them fire while a text field has the keyboard.

## Documentation

- [docs/USER_GUIDE.md](docs/USER_GUIDE.md) — every feature, in the order you
  would meet it, plus troubleshooting for each thing the app can tell you
- [docs/CONFIGURATION.md](docs/CONFIGURATION.md) — every setting, its default
  and range, every file fastcription reads or writes, and what the tuning
  knobs actually do
- [docs/SERVER.md](docs/SERVER.md) — running the model on another computer
- [docs/OVERLAY.md](docs/OVERLAY.md) — the one compositor rule compact mode
  wants
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — the design, what was measured,
  and which voxtype interfaces are relied on

## How it works

fastcription captures the chosen source itself and, about once a second,
re-transcribes the sentence being spoken from its start with
`voxtype transcribe`. Words are published once two consecutive passes agree on
them, which puts text on screen about one and a half to two seconds behind the
speaker without it flickering as it is read. Each finished sentence is written
to SQLite as it is confirmed, so a crash costs one sentence rather than the
meeting.

## License

MIT.
