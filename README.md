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
- **Shows the transcript live**, in the main window or in a borderless
  always-on-top caption bar that sits over the meeting.
- **Optionally captures your microphone too**, as a second track, so the
  transcript records both sides of the conversation.
- **Keeps a library**: conversations with custom names, grouped and tagged,
  searchable by what was actually said — not just by title — and exported to
  text, Markdown, JSON, SRT or WebVTT.
- **Imports past voxtype meetings** recorded by voxtype's own meeting mode.
- **Starts and stops the voxtype systemd user service** from the UI.

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

The always-on-top caption overlay needs one window rule on Wayland, because
winit cannot ask the compositor to keep a window above others.
[docs/OVERLAY.md](docs/OVERLAY.md) has it for Hyprland, sway and river.

## How it works

fastcription captures the chosen source itself, cuts it into chunks on silence
boundaries, and hands each chunk to `voxtype transcribe`. On a modern desktop
CPU a seven-second chunk comes back in under a second, so the transcript trails
live audio by about that much. Segments are written to SQLite as they are
confirmed, so a crash costs one chunk rather than the meeting.

[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) has the full design, including what
was measured, which voxtype interfaces are relied on, and the two upstream
patches that would let fastcription drop its own chunker.

## License

MIT.
