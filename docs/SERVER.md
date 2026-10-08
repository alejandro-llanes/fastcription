# Running the model on another computer

fastcription can send each transcription pass to a server instead of running
the model locally. The usual reason is hardware: a desktop with a GPU can serve
a laptop that has none, and because the model stays resident there, no pass
pays to load it. The laptop does nothing but capture audio and make an HTTP
request.

The short version, for someone who already has a GPU box:

1. Install CUDA and build whisper.cpp with it.
2. Download `large-v3-turbo`.
3. Run `whisper-server` with `--inference-path /v1/audio/transcriptions`.
4. Reach it over Tailscale, or open the port on your LAN.
5. In fastcription: **Settings → Transcription server**, tick **Run the model
   on another computer**, enter the address, press **Test connection**.

Each step is below.

## The protocol

voxtype's remote mode speaks the OpenAI audio transcription API. For an
endpoint of `http://desktop.lan:8080` it sends:

```
POST http://desktop.lan:8080/v1/audio/transcriptions
Content-Type: multipart/form-data
Authorization: Bearer <key, if set>

fields: file=<audio.wav>, model, language, response_format
```

and reads the `text` field of the JSON reply. Any server implementing that
works. This was verified against voxtype 1.0.1's source and is exercised
end-to-end against a mock server in fastcription's own test suite
(`a_transcription_server_is_reached_through_the_config_we_write`).

## 1. CUDA

**An RTX 50-series card needs CUDA 12.8 or newer.** These are Blackwell, compute
capability 12.0 (`sm_120`), and no earlier toolkit can generate code for it.
Check what your card reports:

```sh
nvidia-smi --query-gpu=name,memory.total,compute_cap --format=csv,noheader
# NVIDIA GeForce RTX 5070, 12227 MiB, 12.0
```

On Arch (and Omarchy) the packaged toolkit is well past that floor:

```sh
sudo pacman -S cuda
```

Other distributions: <https://developer.nvidia.com/cuda-downloads>. If `nvcc`
is not on your `PATH` afterwards, it is usually at `/opt/cuda/bin/nvcc`.

## 2. Build whisper.cpp with CUDA

```sh
git clone https://github.com/ggerganov/whisper.cpp
cd whisper.cpp
cmake -B build -DGGML_CUDA=1 -DCMAKE_CUDA_ARCHITECTURES=120
cmake --build build -j --config Release
```

`CMAKE_CUDA_ARCHITECTURES=120` is compute capability 12.0 without the dot — the
RTX 50-series. whisper.cpp's own README shows `86` in this example, which is
Ampere (RTX 30-series); building with that on a Blackwell card leaves you
running PTX compiled for a different architecture, or failing outright. Use the
number your card reported above: `120` for RTX 50-series, `89` for RTX 40,
`86` for RTX 30.

Confirm the build found the GPU — `whisper-cli` prints the backend it loaded:

```sh
./build/bin/whisper-cli -m models/ggml-large-v3-turbo.bin -f samples/jfk.wav 2>&1 | grep -i cuda
```

## 3. Download a model

whisper.cpp ships a script that fetches from
<https://huggingface.co/ggerganov/whisper.cpp>:

```sh
sh ./models/download-ggml-model.sh large-v3-turbo
```

It lands in `models/ggml-large-v3-turbo.bin`. To download by hand, or onto a
machine without the repository checked out, the files are at
`https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-<name>.bin`:

```sh
curl -L -o ggml-large-v3-turbo.bin \
  https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin
```

Sizes, read from the HuggingFace repository:

| Model | Size | |
| --- | --- | --- |
| `tiny.en` | 78 MB | English only |
| `base.en` | 148 MB | English only; what fastcription uses locally |
| `small.en-q5_1` | 190 MB | English only, quantised |
| `small.en` | 488 MB | English only |
| `medium.en-q5_0` | 539 MB | English only, quantised |
| **`large-v3-turbo-q5_0`** | **574 MB** | quantised turbo |
| `large-v3-turbo-q8_0` | 874 MB | quantised turbo |
| `large-v3-q5_0` | 1081 MB | quantised full |
| `medium.en` | 1534 MB | English only |
| **`large-v3-turbo`** | **1625 MB** | **the one to use on a 12 GB card** |
| `large-v3` | 3095 MB | the full model |

## 4. Which model for an RTX 5070

**`large-v3-turbo`.** With 12 GB of VRAM there is no reason to quantise and no
reason to reach for anything smaller.

Turbo is a pruned `large-v3`: the decoder drops from 32 layers to 4 while the
encoder is left at full size, which takes it from 1.54 billion parameters to
809 million. The encoder does the heavy audio work and the decoder is what the
latency depends on, so the asymmetry is the point — it runs roughly four times
faster on a GPU for about one to two WER points on English
([comparison](https://vexascribe.com/whisper-large-v3-vs-turbo),
[write-up](https://medium.com/@bnjmn_marie/whisper-large-v3-turbo-as-good-as-large-v2-but-6x-faster-97f0803fa933)).
That trade is exactly the right way round for live captions.

Reasons to pick something else:

- **`large-v3`** if you are transcribing a recording afterwards and want the
  last point of accuracy more than you want speed. At 3 GB it still fits
  comfortably.
- **`large-v3-turbo-q5_0`** (574 MB) if the GPU is also running something else
  and you want the VRAM back. Quantisation costs a little accuracy.
- **`medium.en`** or **`small.en`** only if your meetings are always English and
  you are short of VRAM. On a 12 GB card they are a downgrade for nothing.

Multilingual models (everything without `.en`) handle English fine; the `.en`
variants are simply better at English for their size. Turbo is multilingual.

**This section is reasoned from the model architecture and published
comparisons, not measured on your card.** Once it is running, the honest
measurement is the one that matters: watch the **transcription behind**
indicator in fastcription's status bar during a real meeting. If it never
appears, the server is keeping up.

## 5. Run the server

```sh
./build/bin/whisper-server \
  --model models/ggml-large-v3-turbo.bin \
  --host 0.0.0.0 \
  --port 8080 \
  --inference-path /v1/audio/transcriptions
```

**`--inference-path` is not optional.** `whisper-server` serves `/inference` by
default, which is not where voxtype posts; without it every request is a 404 and
**Test connection** fails. **`--host 0.0.0.0`** is what makes it reachable from
another machine — the default binds to localhost only.

To keep it running, a user unit at
`~/.config/systemd/user/whisper-server.service`:

```ini
[Unit]
Description=whisper.cpp server for fastcription
After=network-online.target

[Service]
ExecStart=%h/whisper.cpp/build/bin/whisper-server \
  --model %h/whisper.cpp/models/ggml-large-v3-turbo.bin \
  --host 0.0.0.0 --port 8080 \
  --inference-path /v1/audio/transcriptions
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
```

```sh
systemctl --user daemon-reload
systemctl --user enable --now whisper-server
systemctl --user status whisper-server
```

A user unit only runs while you are logged in. For a headless box, either
`sudo loginctl enable-linger $USER` or install it as a system unit under
`/etc/systemd/system/` with a `User=` line.

## 6. Reaching it from the laptop

### Tailscale, if you have it

The better answer, and it needs no firewall change at all. Both machines join
your tailnet, the traffic is encrypted end to end, and it works from anywhere
rather than only on your LAN. Use the server's tailnet address in fastcription:

```sh
tailscale ip -4        # on the server, e.g. 100.100.100.10
tailscale status       # the machine names work too, e.g. http://desktop:8080
```

Tailscale traffic bypasses the host firewall rules below, so if you only ever
connect this way there is no port to open.

### Opening the port on your LAN

Find the server's address first:

```sh
ip -4 -o addr show scope global | awk '{print $2, $4}'   # e.g. enp3s0 192.168.1.50/24
```

Open 8080 **to your local network only**, never to the whole world — this
server takes audio from anyone who can reach it, and unless you set an API key
it takes it from them unauthenticated.

**ufw** (what Omarchy manages; check yours with `sudo ufw status verbose`):

```sh
sudo ufw allow from 192.168.1.0/24 to any port 8080 proto tcp comment 'whisper-server'
sudo ufw status verbose
```

**firewalld**:

```sh
sudo firewall-cmd --permanent --zone=home --add-port=8080/tcp
sudo firewall-cmd --reload
```

**nftables** directly:

```sh
sudo nft add rule inet filter input ip saddr 192.168.1.0/24 tcp dport 8080 accept
```

**iptables** directly:

```sh
sudo iptables -A INPUT -s 192.168.1.0/24 -p tcp --dport 8080 -j ACCEPT
```

`nft` and `iptables` rules are lost on reboot unless your distribution saves
them (`iptables-save`, `nftables.service`); `ufw` and `firewalld` persist their
own.

Check from the laptop before touching fastcription:

```sh
curl -sS -o /dev/null -w '%{http_code}\n' http://192.168.1.50:8080/
```

Anything other than a connection error means the port is open.

## 7. Configure fastcription

**Settings → Transcription server**, then:

| Field | What to put in it |
| --- | --- |
| **Run the model on another computer** | Tick it. The local engine, model, language, fast mode and thread controls grey out, because none of them apply to a server. |
| **Address** | The scheme, host and port, and nothing else: `http://192.168.1.50:8080` or `http://desktop:8080`. fastcription appends `/v1/audio/transcriptions` itself. |
| **Model** | Whatever name the server answers to. `whisper-1` is the default and whisper.cpp accepts it for whichever model it has loaded, so leave it unless your server is strict. |
| **API key** | Leave empty for whisper.cpp, which has no authentication. Fill it in for a server that wants a bearer token. |
| **timeout s** | 30 by default, 5 to 120. Raise it on a slow link or a busy GPU; a pass that exceeds it is reported and retried. |

Then press **Test connection**. It transcribes half a second of silence through
the real path — the address, the multipart body and the token — so a typo turns
up now rather than during a meeting. It runs on its own thread, so the window
stays responsive while it waits, and it writes to a temporary file rather than
the config a running session is using.

The address, the model and the timeout are remembered across launches. **The
API key is not** — egui's storage is a plain-text file, so the key is kept for
the session only and re-entered next time. While a session runs it lives in
`~/.config/fastcription/voxtype-<session start in ms>.toml`, created readable
only by you and removed when the session ends.

## 8. Network and privacy

Audio leaves the machine. Over plain `http://` it crosses the network
unencrypted, which fastcription warns about in the settings pane and voxtype
warns about in its log. On a home network that is usually an acceptable trade;
otherwise use Tailscale as above, or put the server behind a TLS reverse proxy
and use `https://`.

Each pass uploads the whole utterance so far, because that is what makes the
transcript accurate — roughly 32 KB per second of speech, so a 20 second
utterance is about 640 KB per pass. On a wired or decent wireless network that
is unremarkable; over a slow link, raise **seconds between passes** to trade
latency for bandwidth.

## 9. When the server is unreachable

The utterance being spoken stays buffered and the pass is retried, so a brief
outage — a server restart, a moment of bad wireless — costs nothing but a pause
in the transcript. After three failed attempts to finish that utterance,
fastcription gives up on it, keeps whatever words had already been agreed,
reports the error, and starts the next one clean. That bound exists so a server
that has gone away for good cannot hold audio in memory; the buffer is also
capped at **longest utterance in seconds** between attempts, so no retry
uploads a larger recording than the one before it.

## 10. When something is wrong

| What you see | What it means |
| --- | --- |
| **Test connection** fails with a 404 | `--inference-path /v1/audio/transcriptions` is missing from the server command. |
| Connection refused from the laptop, fine on the server | The server is bound to localhost (`--host 0.0.0.0` missing), or the port is closed in the firewall. |
| Connection times out rather than refusing | A firewall is dropping the packets. A refusal means nothing is listening; a timeout means something is silently discarding. |
| 401 or 403 | The server wants a bearer token and the **API key** field is empty, or has the wrong one. |
| Transcripts arrive but are wrong or empty | The server loaded a different model than you think. Check its startup log. |
| **transcription behind** keeps appearing | The server cannot keep up. Try `large-v3-turbo` if you are on `large-v3`, check the GPU is actually being used, or raise **seconds between passes**. |
| It works, but the GPU sits idle | whisper.cpp was built without `-DGGML_CUDA=1`, or for the wrong `CMAKE_CUDA_ARCHITECTURES`. Rebuild and check `whisper-cli`'s backend line. |

## Other servers

Anything implementing the OpenAI transcription endpoint works; fastcription
cannot tell the difference. Projects such as `speaches` (formerly
faster-whisper-server) expose `/v1/audio/transcriptions` natively, so there is
no path to remap, and ship CUDA container images. Use whichever you would
rather maintain.
