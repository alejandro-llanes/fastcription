# Installing fastcription and everything it talks to

fastcription itself is one binary. What takes setting up is the machinery it
drives: **voxtype**, which does the transcription, and — optionally — a
**whisper server on a GPU** to make it fast, and **Ollama** to explain words
you did not know.

This page goes through them in the order they matter. Only the first two are
needed to use the app at all.

| | What for | Needed? |
| --- | --- | --- |
| [PipeWire or PulseAudio](#1-the-sound-server) | Capturing the audio | **Yes** |
| [voxtype](#2-voxtype) | Turning it into text | **Yes** |
| [fastcription](#3-fastcription) | The window | **Yes** |
| [A GPU for whisper](#4-optional-transcription-on-a-gpu) | Making it keep up | Optional, and transformative |
| [Ollama](#5-optional-meanings-for-words-you-did-not-know) | The word registry | Optional |

---

## 1. The sound server

fastcription enumerates sources with `pactl` and captures with `parec`, so it
needs PipeWire (with its Pulse shim) or PulseAudio.

```sh
pactl info | head -2        # should name a server
parec --help >/dev/null     # should exist
```

On Arch both are in `libpulse`; on Debian and Ubuntu, `pulseaudio-utils`.
Nothing else is required — fastcription does not link libpipewire, it runs
these two commands.

## 2. voxtype

[voxtype](https://github.com/peteonrails/voxtype) is the transcription engine.
fastcription runs `voxtype transcribe` once a second against the sentence being
spoken; it never talks to voxtype's daemon, so you do **not** need the systemd
service for fastcription's sake. Install it from its own project page, then:

```sh
voxtype --version            # verified against 1.0.1
voxtype setup --download     # fetch a model (base.en is a good start)
voxtype info models          # confirm one says "installed"
```

**Make it use your GPU if you have one.** voxtype ships prebuilt variants and
switches between them itself. Its Whisper engine accelerates through **Vulkan**
(CUDA is for its Parakeet engine):

```sh
voxtype setup gpu --status   # what is active, and what is available
voxtype setup gpu --enable   # switch to the best backend it finds
```

If `--status` lists more than one GPU — a desktop with onboard graphics has at
least two — selection is "first available", which is usually the slow one. Pin
it:

```ini
# ~/.config/systemd/user/voxtype.service.d/gpu.conf
[Service]
Environment="VOXTYPE_VULKAN_DEVICE=nvidia"
```

`voxtype info accel` confirms what is actually in play. fastcription shows the
same thing in its readiness checklist and warns when it reads `cpu-only`,
because a CPU-only engine cannot produce live captions — see the numbers in
[§4](#4-optional-transcription-on-a-gpu).

## 3. fastcription

```sh
curl -fsSL https://raw.githubusercontent.com/alejandro-llanes/fastcription/master/install.sh | sh
```

It installs into `~/.local` — binary, menu entry and icon — verifies the
download against its published SHA-256, needs no root, and finishes by telling
you which of the above is missing. `FASTCRIPTION_VERSION`, `PREFIX`, `BIN_DIR`
and `NO_DESKTOP` change what it does; read it first if you would rather not
pipe a script into a shell:

```sh
curl -fsSL https://raw.githubusercontent.com/alejandro-llanes/fastcription/master/install.sh -o install.sh
less install.sh && sh install.sh
```

### Build from source

Rust 1.98 or newer, plus the X11/Wayland/GL headers the windowing layer needs:

```sh
# Arch
sudo pacman -S --needed rust libxkbcommon wayland libx11 libxcursor libxi libxrandr mesa
# Debian / Ubuntu
sudo apt install build-essential pkg-config libxkbcommon-dev libwayland-dev \
     libx11-dev libxcursor-dev libxi-dev libxrandr-dev libgl1-mesa-dev libegl1-mesa-dev

cargo build --release -p fastcription
install -Dm755 target/release/fastcription ~/.local/bin/fastcription
install -Dm644 packaging/fastcription.desktop ~/.local/share/applications/fastcription.desktop
install -Dm644 packaging/fastcription.svg ~/.local/share/icons/hicolor/scalable/apps/fastcription.svg
```

The first build compiles egui and winit from the forks fastframe pins, which
takes a while.

### Uninstall

```sh
rm -f ~/.local/bin/fastcription \
      ~/.local/share/applications/fastcription.desktop \
      ~/.local/share/icons/hicolor/scalable/apps/fastcription.svg
rm -rf ~/.local/share/fastcription        # the library and settings
rm -rf ~/.config/fastcription             # themes and per-session configs
```

## 4. Optional: transcription on a GPU

This is the difference between a toy and a tool, and it is worth being precise
about why. Measured on an RTX 5070 with `large-v3-turbo`, encoding a 7-second
window — the work one pass of live captioning does:

| Backend | Encode per pass | |
| --- | --- | --- |
| **CUDA** | **85 ms** | |
| CPU, 24 threads | 2,796 ms | 33× slower |
| CPU, 4 threads (the default) | 10,256 ms | 121× slower |

A pass has about a second to finish. On the CPU it does not, so the captions
fall behind and keep falling — no amount of tuning the pass interval rescues
it, because the problem is not the schedule. If realtime feels hopeless, this
is almost always why.

Two ways to get there:

- **voxtype's own GPU variant**, [§2](#2-voxtype) — simplest, and enough for
  most machines.
- **A whisper server**, so a desktop with a GPU can serve a laptop without
  one. On Arch that is two packages, not a source build:

  ```sh
  sudo pacman -S whisper-cpp ggml-cuda      # whisper-cpp alone is CPU-only
  ```

  [SERVER.md](SERVER.md) is the full walkthrough: which model, how to tell from
  the startup log whether CUDA is really in play, opening the port, and what to
  put in **Settings → Server**.

## 5. Optional: meanings for words you did not know

The word registry asks a small language model what an expression means in the
line it was said in, and what it is in your language. That is an
[Ollama](https://ollama.com) server, usually the same machine as the whisper
one:

```sh
sudo pacman -S ollama-cuda          # or ollama, for CPU
sudo systemctl enable --now ollama
ollama pull gemma3:4b
```

Then **Settings → Words**: the address (`http://127.0.0.1:11434` locally),
the model, and your language. **Test connection** says whether the model is
pulled.

### Which model

Measured on the same RTX 5070, with whisper-server already resident at 2.1 GB,
using the exact request the registry sends:

| Model | VRAM, both resident | Lookup | tok/s | The Spanish |
| --- | --- | --- | --- | --- |
| qwen2.5:1.5b | 3.4 GB | 0.19 s | 304 | "boil the ocean" → *hervir el mar*. Literal, meaningless |
| qwen2.5:3b | 4.4 GB | 0.20 s | 205 | "ballpark figure" → *figure de aproximadamente*. Broken |
| **gemma3:4b** | **6.0 GB** | **0.41 s** | 142 | *intentar hacer demasiado a la vez*. Clean JSON every time |

All three ran entirely on the GPU and all three were fast enough. The two
quicker ones would mislead a reader, which is the opposite of the point, so
`gemma3:4b` is the default. Cold start is about five seconds, which is why
**Settings → Words → Keep it ready** asks Ollama to hold the model in memory.

To reach it from another machine, have Ollama listen beyond localhost
(`Environment="OLLAMA_HOST=0.0.0.0"` via `systemctl edit ollama`) and open port
11434 the way [SERVER.md](SERVER.md) §6 opens 8080.

---

## Checking it all works

Open fastcription. The **Live** pane is a readiness checklist until something
has been recorded, and it tells you exactly which of the above is missing, with
the command to fix it on a button:

- **voxtype** — the path to the binary
- **model** — `base.en · 3 installed`
- **audio sources** — `12 to choose from`
- **library** — where the transcripts go
- **speed** — the backend, and a warning if it is the CPU

When all of them pass it collapses to one line, and you are ready to press
Start. If something is wrong later, [USER_GUIDE.md §18](USER_GUIDE.md) has
every message the app can show and what to do about it.
