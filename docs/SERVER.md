# Running the model on another computer

fastcription can send each transcription pass to a server instead of running
the model locally. The usual reason is hardware: a desktop with a GPU can serve
a laptop that has none, and because the model stays resident there, no pass pays
to load it.

Turn it on under **Settings → Transcription server**, enter the address, and
press **Test connection** before you rely on it.

## The protocol

voxtype's remote mode speaks the OpenAI audio transcription API. For an endpoint
of `http://desktop.lan:8080` it sends:

```
POST http://desktop.lan:8080/v1/audio/transcriptions
Content-Type: multipart/form-data
Authorization: Bearer <key, if set>

fields: file=<audio.wav>, model, language, response_format
```

and reads the `text` field of the JSON reply. Any server implementing that
works. This was verified against voxtype 1.0.1's source and exercised
end-to-end against a mock server in fastcription's own test suite
(`a_transcription_server_is_reached_through_the_config_we_write`).

## Option 1: whisper.cpp's server

Lightweight, and the same engine family as the local path.

```sh
# On the machine with the GPU
cmake -B build -DGGML_CUDA=1
cmake --build build -j --config Release
./build/bin/whisper-server \
  --model models/ggml-large-v3-turbo.bin \
  --host 0.0.0.0 --port 8080 \
  --inference-path /v1/audio/transcriptions
```

**`--inference-path` is not optional here.** `whisper-server` serves
`/inference` by default, which is not where voxtype posts. Without that flag
every request returns 404 and the connection test fails.

## Option 2: an OpenAI-compatible server

Projects such as `speaches` (formerly faster-whisper-server) expose
`/v1/audio/transcriptions` natively and ship CUDA container images, so there is
no path to remap. Use whichever you would rather maintain; fastcription cannot
tell the difference.

## Which model

With a GPU doing the work, the reason to stay on a small model disappears.
`large-v3-turbo` is the usual choice: far more accurate than `base.en`, and on a
recent card fast enough that the round trip is dominated by the network rather
than by inference.

Set the **Model** field to whatever name that server uses for its loaded model
(`whisper-1` is a common default that many servers accept for whatever they
have).

## Network and privacy

Audio leaves the machine. Over plain `http://` it crosses the network
unencrypted, which fastcription warns about in the settings pane and voxtype
warns about in its log. On a home network that is usually an acceptable trade;
otherwise put it behind a tunnel (WireGuard, SSH forward) or a TLS reverse
proxy and use `https://`.

Each pass uploads the whole utterance so far, because that is what makes the
transcript accurate — roughly 32 KB per second of speech, so a 20 second
utterance is about 640 KB per pass. On a wired or decent wireless network that
is unremarkable; over a slow link, raise **seconds between passes** to trade
latency for bandwidth.

If an API key is set, it is written into
`~/.config/fastcription/voxtype.toml`, and that file is then readable only by
you.

## When the server is unreachable

The utterance being spoken stays buffered and the pass is retried, so a brief
outage — a server restart, a moment of bad wireless — costs nothing but a pause
in the transcript. After three consecutive failures fastcription gives up on
that utterance, keeps whatever words had already been agreed, reports the error,
and starts the next one clean. That bound exists so a server that has gone away
for good cannot hold audio in memory or make every retry upload a larger
recording.
