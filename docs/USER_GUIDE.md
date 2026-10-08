# fastcription — User guide

## 1. What it is for

If you follow English better read than heard, a meeting goes past you faster
than you can keep up. fastcription captures one audio source you choose — the
call, and nothing else on the machine — transcribes it with
[voxtype](https://github.com/peteonrails/voxtype), and shows the words on
screen about one and a half to two seconds behind the speaker, while the
meeting is still happening. Afterwards the transcript stays in a searchable
library you can name, group, tag and export.

## 2. Requirements and installation

You need:

- **Linux with PipeWire or PulseAudio**, with `pactl` and `parec` on `PATH`.
  fastcription enumerates with `pactl -f json list sources` and
  `pactl -f json list sink-inputs`, and captures with
  `parec --raw --format=s16le --rate=16000 --channels=1`.
- **voxtype, with at least one model installed.** fastcription looks for
  `voxtype` on `PATH` and falls back to `/usr/bin/voxtype`. To fetch a model:

  ```sh
  voxtype setup --download
  ```

  Verified against voxtype 1.0.1. The voxtype *daemon* is not needed —
  fastcription runs `voxtype transcribe` per pass and never talks to the
  service (see section 14).
- **Rust 1.98 or newer** to build.

Build and run:

```sh
cargo run -p fastcription
```

The first build compiles egui and winit from the forks fastframe pins, which
takes a while.

For a menu entry, put the release binary on `PATH` and copy the desktop file:

```sh
cp packaging/fastcription.desktop ~/.local/share/applications/
```

It runs `fastcription`, declares `StartupWMClass=fastcription`, and asks for
an icon named `fastcription` — fastcription ships no icon file, so your theme
will fall back to a generic one.

Compact mode (section 7) needs nothing configured on Hyprland: fastcription
asks the compositor for the caption bar's shape itself. On other compositors it
changes what is drawn and leaves the window alone, and
[OVERLAY.md](OVERLAY.md) has the rules that give it a shape there.

## 3. First run

The window opens at once on defaults. The source list, the engine and model
catalogue and voxtype's own configured defaults are read by subprocesses on a
background thread and fill in a moment later.

With nothing recorded yet, the **Live** pane is a readiness checklist instead
of an empty transcript. Four checks say whether a recording can start, in the
order you would fix them, and a fifth says whether it will be fast enough to
read along with.

| Check | Met shows | Unmet shows |
| --- | --- | --- |
| **voxtype** | the path to the binary | "voxtype was not found on PATH, so nothing can be transcribed. Install it and run `voxtype setup --download` to fetch a model." |
| **model** | `base.en · 3 installed`, or `base.en (voxtype did not list its models)` | "No transcription model is installed. Run `voxtype setup model` to download one." |
| **audio sources** | `12 to choose from` | "No sound server answered, so there is nothing to record from. Check that PipeWire or PulseAudio is running." |
| **library** | the path to `library.db` | "The conversation library at … could not be opened, so recording is disabled.", or "The conversation library at … will not accept writes, so nothing can be recorded. Past transcripts are still readable and exportable." |
| **speed** | the backend, e.g. `GPU (Vulkan)` | "voxtype is transcribing on this machine's CPU, which cannot keep up with speech — the captions will fall behind and keep falling. Turn on GPU acceleration, or send the audio to a machine that has a GPU (Settings → Transcription server)." |

Each unmet row carries a button so you do not have to retype anything:

- **Copy command** on the voxtype row copies `voxtype setup --download`
- **Copy command** on the model row copies `voxtype setup model`
- **Copy command** on the audio row copies `systemctl --user status pipewire wireplumber`
- **Copy path** on the library row copies the path to the library file
- **Copy command** on the speed row copies `voxtype setup gpu --enable`

The model check passes on a name typed into Settings even when
`voxtype info models` could not be read, because transcription works perfectly
well while that probe fails.

**The Waiting state.** While the startup probes are still running, the model
and audio rows show a spinner and the word `looking…` rather than a warning.
Those two answers come from subprocesses: for the first fraction of a second
of a launch both lists are genuinely empty, and calling that "no sound server"
would be a complaint that corrects itself.

**The speed check is a warning, not a requirement.** Recording works on the
CPU and the transcript is correct; it is the *latency* that fails, and with it
the only thing this application is for. On one desktop a 7-second window of
speech took 85 ms to encode on a GPU and 2.8 seconds on 24 CPU threads, against
a budget of about a second per pass — so the captions arrive after the
conversation has moved on, and the gap grows for as long as the meeting lasts.
`docs/SERVER.md` covers both remedies.

It is skipped in two cases, because this machine's backend then says nothing
about the latency: when a transcription server is configured, since the model
runs there; and when voxtype reported no backend at all, which is what happens
while its daemon is not running.

When the four requirements pass, the checklist collapses to one line:

> Ready — choose a source and press Start (Ctrl+R)

A speed warning does not hold the checklist open — it appears underneath that
line, so "Ready" never hides the fact that the captions will not keep up.

## 4. Choosing what to transcribe

The source picker sits in the top bar, and again under **Audio source** in
Settings; they are the same control. It offers three kinds of thing:

- **Capture devices** — a microphone, a line input, a virtual source. Listed
  by the sound server's own description.
- **Sink monitors** — the monitor of an output sink, which is everything the
  system plays through it. These appear with names like
  `Monitor of Built-in Audio Analogue Stereo`.
- **Single applications** — one application's playback stream, captured on its
  own through `parec --monitor-stream=<index>`. This is the most useful option
  for a meeting: it transcribes the call without picking up your music. These
  are listed by the application that owns them — `application.name`, falling
  back to the process binary and then to the stream's title — with the stream's
  own title (`media.name`) appended when it differs, so two streams of one
  application can be told apart. fastcription's own streams are left out of the
  list.

Entries that `pactl` reports in a shape fastcription cannot read are dropped
individually rather than failing the whole list, so one malformed stream never
hides every other source.

The **refresh** button beside the picker ("Look for audio sources again")
re-enumerates. It matters because application streams come and go: the
application whose audio you want may not have started playing when
fastcription launched. A refresh keeps your choice selected if it is still
there — matched by identity (kind, source name, application), not by position,
because a source that disappears shifts every index after it.

The picker is **disabled while a session runs**, with the tooltip "Stop the
recording to change the source". The pipeline resolves its source when it
starts and never looks again, so a picker that still moved would claim
fastcription was recording something it was not.

**If the chosen source disappears**, nothing is selected and you are told:

> Monitor of Built-in Audio Analogue Stereo is not available any more, so nothing is selected to record.

fastcription never falls back to whatever is listed first. The same applies to
the source restored from your last launch: if it is gone when the source list
arrives, you get that notice and an empty picker.

Mid-session the capture thread handles this itself. It re-resolves the source,
reports `Audio source lost: …` as a warning and keeps retrying with a backoff
from 250 ms up to 4 seconds; when audio flows again that one warning is taken
away and replaced with "The audio source is back." The transcript is kept
across the gap. A source delivering no frames at all for 5 seconds raises
"no audio received from … for 5s; it may be suspended"; after 15 seconds the
capture is restarted.

## 5. Recording

### The top bar

Left to right: the **wordmark**, then a **state light** — a coloured dot and
one word, `REC`, `PAUSED`, `FINISHING` or `IDLE`, the dot haloed while
recording — then the **source picker** and its refresh button, then the big
round **record button** and a small **Stop** beside it, then the **Mic**
toggle. On the right, **Live / Settings** as one switch, and the button that
enters compact mode.

The record button is one control that starts, pauses and resumes, like a
player's: the icon says which it will do next, and it is lit whenever
pressing it would do something. It is the largest thing on the bar on
purpose — it is the control the hand goes to without looking. Mic lights up
the same way when the second track is on.

**Start** (`Ctrl+R`) begins a new conversation. It is refused, with a notice,
if there is no source selected ("Choose an audio source first."), no voxtype,
no model, or a library that will not take writes. The Start button is greyed
out entirely on a read-only library.

**Pause** (`Ctrl+Space`) and **Resume** (`Ctrl+R` or `Ctrl+Space`) suspend and
continue. The Start button's label becomes **Resume** while paused.

**Stop** (`Ctrl+.`) ends the conversation.

The shortcuts are in each button's tooltip rather than its label, so three
buttons and a source picker still fit across the narrowest window.

### The window title

On a tiled desktop the title is often the only part of fastcription you can
see while looking at the call, so it says what the session is doing:

| State | Title |
| --- | --- |
| Recording | `● REC 12:34 — fastcription` |
| Paused | `⏸ Paused — fastcription` |
| Finishing | `Finishing — fastcription` |
| Idle | `fastcription` |

The clock there is the recording time, truncated to the second. Compact mode
owns the title while it is on, and none of these appear — see
[OVERLAY.md](OVERLAY.md) for why.

### The status bar

The spectrum of the *selected* source is drawn across the whole foot of the
window, and the footer sits over it, translucent, with the loud bands
breaking out of the top. It deliberately does not read your microphone: the
side you cannot hear is the one you need to see arriving. Unticking "Show it
in the full window too" under **Settings → Appearance** puts a percentage
meter here instead.

Nothing on the bar is read straight off the spectrum. The readouts sit on an
opaque plate and each state word on its own chip, which is what lets the bar
be see-through at all — labels drawn directly onto it would have needed the
bar to be 96% opaque to stay readable, and a bar you cannot see through is
not worth drawing a spectrum behind.

None of it is a control:

- **ELAPSED** — `HH:MM:SS` of recording time, which stops while paused, and
  brightens while it is running. Hovering says "How long this conversation has
  been recording".
- **SOURCE** — what is being recorded, shortened to fit. It is a reminder of a
  choice already made; the control that makes it is in the top bar.
- **transcription behind** — a chip, and only while transcription cannot keep
  up. See section 18.
- **voxtype** — the daemon's state, as a chip on the right. Hidden while a
  transcription server is in use: the daemon's state then says nothing about
  what is transcribing, and a chip reading "stopped" beside a transcript that
  is arriving fine only invites you to start something you do not need. The
  Start/Stop controls stay under **Settings → System**.
- **"transcription behind — words arrive late"** — appears only while
  transcription cannot keep up, that is while one pass takes longer than the
  **seconds between passes** setting. No audio is lost when this shows; the
  interval stretches and words arrive later. See section 18.
- **The service pill** — `voxtype: running`, `voxtype: stopped` or
  `voxtype: unknown`, re-checked automatically. This is about voxtype's
  dictation daemon, not about this app's transcription (section 14).

### What Pause does to the sentence in flight

Audio that arrives while paused is discarded, matching what voxtype's own
meeting pause does. So pausing also **cuts the utterance in flight**: the
sentence you had reached is finalised there and becomes its own line, and
resuming starts a fresh one. Without that cut, the sentence before the pause
and the one after it would be welded into a single line reading as though they
had been said together, because the gap is closed rather than preserved.

A consequence worth knowing: timestamps in the transcript measure *speech*,
not the wall clock. Pause for ten minutes and the transcript carries on where
it left off.

### What Stop does, and the Finishing state

Stop ends capture immediately but the audio already recorded still has to be
transcribed. The window shows **Finishing** at once and the wait happens on a
background thread, so it does not freeze at the moment you are watching for
your last words. The Live pane says "Transcribing the last of the audio…".
While Finishing, Start, Pause and Stop are all inert — starting a second
session would take the conversation row the first one is still writing to.

When the backlog is through, the conversation is closed and the library
reloads, so its row shows its real duration. If any lines could not be written
to the library the conversation is marked **interrupted** instead of completed,
and you are told how many went missing.

## 6. Reading the transcript

The Live pane scrolls up from the bottom and stays stuck there unless you
scroll away.

**Settled and unsettled words.** Every pass re-transcribes the sentence being
spoken from its start, and a word is published once two consecutive passes
agree on it. Settled words are drawn in the ordinary text colour and are never
revised afterwards. The tail of the current pass — the newest words on screen,
still being revised — is drawn *in italics*, in the palette's secondary
colour, at the same size. Italics rather than a fainter colour, because the
newest words are the ones you are actually reading.

**The pulsing marker.** A dot in the left gutter breathes on and off, about
0.8 seconds each way, beside the line still arriving. It replaced the word
"provisional", which read as part of the transcript.

**Text size.** Everything is drawn at the size you choose: 14 to 40 points,
default 22. There are three ways to change it, and they all move the same
setting:

- the **size** slider in the Live pane header
- the **text size** slider in Settings
- `Ctrl+=` (or `Ctrl++`) to grow, `Ctrl+-` to shrink, two points a press, and
  `Ctrl+0` to go back to 22

**Selecting and copying.** Each line is laid out as a single run, so a drag
can start on a speaker name and end two lines down, and the copy carries
everything it crossed. Three buttons and one menu entry:

| | What lands on the clipboard |
| --- | --- |
| **Copy** | the whole transcript as plain text, one line per utterance, as `[mm:ss] Remote: …` |
| **Copy as Markdown** | exactly what an exported `.md` file would hold, with timestamps, speaker labels and a metadata header all on |
| **Copy line** (right-click a line) | that one line, as `[mm:ss] Remote: …` |

The times on the clipboard are `mm:ss`, growing an hour field past the hour
(`1:30:00`). An exported file uses the fuller `HH:MM:SS.mmm` instead.

Copy as Markdown needs the conversation record for its heading, so it is
disabled until there is one; the disabled tooltip says so. Lines holding
nothing that was said — whisper's `[BLANK_AUDIO]`, `[MUSIC]`,
`(music playing)` and bare punctuation — are skipped on the way to the
clipboard.

**Two tracks.** With the microphone track on (section 8), lines from your own
microphone carry a two-pixel accent-coloured bar down the left gutter and are
labelled **You**; lines from the selected source are labelled **Remote**. The
bar is what the eye reads, not the repeated word. The microphone's label takes
the quieter colour on purpose: the side you cannot hear is the one that should
stand out.

**The earlier-lines cap.** The Live pane draws at most the 400 most recent
lines. Above them you will see, for example:

> 182 earlier lines — open this conversation in the sidebar to read them all

Nothing is lost: every line is in the library, and opening the conversation
from the sidebar shows the whole thing. The cap exists because a three-hour
meeting is well over a thousand wrapped lines and laying all of them out every
frame costs real frame time for lines nobody is looking at.

## 7. Compact mode

Compact mode shrinks the window to a caption strip 760 × 170 points, drops its
decorations and asks the compositor to keep it on top, so it can sit over a
fullscreened meeting window.

**To enter it:** the **Compact** button in the top bar, or `Ctrl+M`.
**To leave it:** the strip's own **Restore** button, `Esc`, or `Ctrl+M`
again. The window goes back to the size it had.

The strip shows the last settled lines, then the line being spoken in
italics. Before anything has been said it reads "Waiting for speech…".

Along the foot of the strip, the spectrum runs the full width and the
controls sit over it:

- **The record button**, big and in the centre. One button that starts,
  pauses and resumes: press it to stop transcribing, press it again to carry
  on. The icon says which; it pauses rather than stops, because a toggle has
  to be reversible — pressing it twice leaves one conversation with a gap in
  it, not two conversations.
- **Stop**, small, just to its left.
- **The clock**, `HH:MM:SS` of recording time, on the left.
- **Restore**, on the right.

Everything between them is spectrum, in whatever style you chose under
**Settings → Appearance** and at whatever height you set there. It is the
quickest answer to "is this thing still hearing the call?" when the captions
have not moved for a while. The height is the height of this whole row, so
every point it takes is a point the captions do not get — which is why it is
yours to set.

Compact mode answers every shortcut the full window does — the size keys and
the transport keys — but all of them need keyboard focus, which the
recommended compositor rule deliberately withholds so your meeting keeps it.
That is why the record button is there: reaching the transport used to mean
restoring the window, pressing a button and shrinking again.

**Compact mode can wear its own theme.** Under **Settings → Appearance →
Compact mode**, tick "Give compact mode its own theme" and pick one. The strip
sits over somebody else's call for an hour, where a plainer or darker palette
is often much easier to read off than the one you like to work in. Left
unticked, it follows the main window.

**Compact mode is the main window.** There is one window; hiding fastcription
to the tray hides the captions with it. Stop compact mode before hiding, or
leave the window up.

On Wayland, "always on top" is a hint the compositor may ignore, so compact
On Hyprland nothing needs configuring: fastcription asks the compositor to
float the window, resize it to the caption bar and pin it to every workspace,
and puts back what was there when you leave. On other compositors it changes
only what is drawn, and the window keeps whatever shape it had;
[OVERLAY.md](OVERLAY.md) has rules for sway and river, and explains why a
window rule alone cannot do this on Hyprland.

## 8. The microphone track

By default fastcription transcribes only the source you chose. Turning on
**Mic** in the top bar — or **Capture my microphone as a second track** in
Settings — adds your own voice as a second track, so the transcript becomes a
record of a conversation rather than half of one.

Choose which microphone under **Microphone** in Settings. The default entry is
`Default microphone`, which is PulseAudio's own `@DEFAULT_SOURCE@` alias for
whatever you have set as your input — the only defensible default, since
picking the first device found would silently record the wrong microphone on
any machine with more than one. The dropdown otherwise lists real capture
devices only, never monitors.

Both the toggle and the microphone picker are disabled while a session runs:
the second track is opened when the session starts and not after.

On screen the two tracks interleave in the order they were spoken, the
microphone's lines marked with an accent bar and labelled **You**. A
conversation recorded with the second track on says "Microphone track
captured" in its history pane. Only the selected source drives the level
meter.

## 9. The library

Every conversation is written to a SQLite library as it is recorded — one row
per finished utterance, not one write at the end — so a crash costs the
sentence in flight rather than the meeting.

### The sidebar

Conversations are listed newest first under date headings: **Today**,
**Yesterday**, **This week**, **This month**, **Earlier**. The headings go by
calendar day, not by elapsed time, so a meeting at 23:50 is "Yesterday" at
00:10 the next morning rather than "twenty minutes ago". "This week" is two to
six days back, "This month" is anything older that still falls in the current
calendar month, and "Earlier" is the rest.

Each row carries, on up to three lines:

- the conversation's **title**, with a red `● REC` badge if it is the one
  being recorded now
- a meta line: `[group · ]HH:MM[ · duration]`, for example
  `Work · 14:30 · 12:34`. The group is left off when a group filter is already
  on, since every row would repeat it. A conversation with no end time reads
  `· unfinished` — unless it is the one recording, which the badge already
  says.
- its **tags**, if any

Hovering a row shows the source it was recorded from. Clicking it opens the
conversation in the main pane. The sidebar holds the 200 most recent
conversations, plus any a search found beyond them.

Empty, it says "No conversations yet. Choose a source and press Start." — or
"Nothing matches that search."

### Groups

A conversation belongs to at most one group. The **Groups** row is an **All**
chip plus one chip per group, in alphabetical order.

- **Create**: type into **New group…** and press Enter or **+**.
- **Filter**: click a chip. Click the same chip again, or **All**, to clear it.
- **Rename**: right-click the chip → **Rename**. An inline field appears with
  **Save** and **Cancel**; Enter saves.
- **Delete**: right-click → **Delete group**, then confirm. The dialog says
  what it costs: "Its conversations are kept, and become ungrouped."

Group names are unique ignoring case, so creating one that already exists
fails with "Could not create the group: …".

Assign a conversation to a group from its own page: the **Group** dropdown,
which also offers **Ungrouped**.

### Tags

A conversation can carry any number of tags. The **Tags** row lists every tag
in the library, alphabetically.

- **Create**: type into **New tag…**, Enter or **+**. Tag names are unique
  ignoring case; creating one that already exists quietly gives you the
  existing tag.
- **Filter**: click chips to toggle them. Several selected tags narrow the
  list to conversations carrying **all** of them.
- **Rename** and **Delete**: right-click, as for groups. Deleting asks first,
  and says "Its conversations are kept, and lose the tag."

Tags are drawn in the theme's accent colour. The library can store a colour
per tag, but there is no control in the interface for setting one.

Apply tags to a conversation from its own page: click names under **Tags** to
toggle them.

### A conversation's page

Opening a row replaces the main pane. At the top: the heading
**Conversation**, the word **recording** in red if it is the live one, and on
the right **Copy**, **Copy as Markdown**, **Export** and **Delete**.

Under that, one line of facts: when it started, how long it ran, the source,
the engine and model as `whisper (base.en)`, and its status — `active`,
`completed` or `interrupted`.

Then **Title** (an editable field; the rename is saved as you type), **Group**,
**Tags**, and the full transcript at your chosen text size, with the same
right-click **Copy line** as the live view.

### The recording badge, and why that one cannot be deleted

The conversation being recorded is the one row still growing, and the library
refuses to delete it — the delete would cascade away the live transcript and
leave the session writing to a row that no longer exists. So its page offers no
**Delete** button at all, and if you reach the deletion another way you get:

> That conversation is being recorded right now. Stop the recording first.

Every other deletion goes through the same confirmation dialog, because
deleting a conversation is the one irreversible thing the app does to your
data: "Permanently delete “…”? The transcript cannot be recovered."

### Interrupted conversations

A conversation is **interrupted** rather than **completed** when its transcript
on disk is shorter than the one you watched. Two things cause it:

- **The process died while recording.** At the next launch, any conversation
  still marked active belongs to a run that never closed it, so fastcription
  marks it interrupted and sets its end time from the last segment that made it
  to disk. Everything committed before the crash is intact and readable.
- **The library refused some writes.** The session counts them, marks the
  conversation interrupted and reports how many lines went missing.

Either way the status is there so a truncated export does not read as
authoritative. On a read-only library the first of these cannot be applied —
the fix is itself a write — so a conversation left `active` by a crash stays
that way and is still listed, because hiding it would hide a transcript you
cannot get back any other way.

## 10. Search

The box at the top of the sidebar searches two things at once:

- **titles**, matched as plain text, ignoring case
- **what was actually said**, matched by SQLite's full-text index over every
  transcript in the library, up to 500 matches

Whatever you type is treated as literal words, not as query syntax: `AND`,
`OR`, `NEAR` and unbalanced quotes match themselves. At worst a search finds
nothing; it can never fail with a syntax error.

A row that matched on its transcript carries the **excerpt** that matched
underneath it, with the matching words picked out in the accent colour and the
surrounding text in italics. The excerpt is clickable — "Open the conversation
at these words" — and opens the transcript scrolled to the line those words
were said in.

The search waits **200 ms** after your last keystroke before running, so
typing a word costs one query rather than one per character.

Clearing the box restores the full list.

## 11. Export

**Export** on a conversation's page opens a small window.

### The five formats

| Format | What it is for |
| --- | --- |
| **Text (.txt)** | the words, one line per utterance. The plainest thing to paste into an email or a notes file. |
| **Markdown (.md)** | a document: a heading naming the conversation, an optional facts block, and consecutive lines from one speaker grouped under a bold label. |
| **JSON (.json)** | the whole record, structured: every segment field, the source as its parts rather than a display label, the engine and backend, and both epoch and RFC 3339 spellings of each instant. The only lossless format. |
| **SubRip (.srt)** | subtitles, for playing a recording of the meeting alongside the transcript. Long lines are split into cues of about 84 characters, and overlapping cues are pulled apart, because SubRip has no way to show two at once. |
| **WebVTT (.vtt)** | subtitles for the web. Same cue splitting, but overlap is kept — two people talking over each other is information this format can carry. |

The first three match what `voxtype meeting export` produces, so transcripts
from either tool are interchangeable downstream. The two subtitle formats are
fastcription's own.

Changing the format updates the suggested filename's extension, unless you
have typed a path of your own.

### The three options

- **Timestamps** — adds `[HH:MM:SS.mmm]` in front of each line in Text and
  Markdown. It changes nothing in JSON (which always carries the numbers) or in
  the subtitle formats (whose cue timing is the point).
- **Speaker labels** — adds `You:` / `Remote:` in Text and in the first cue of
  each line in SRT and VTT; in Markdown it groups consecutive lines from one
  speaker under a bold heading. Ignored by JSON, which always carries the
  field.
- **Metadata header** — adds the title, start, end, source and engine as a
  block in Text and Markdown, a `NOTE` block in WebVTT, and the whole
  `metadata` object in JSON. SubRip has no comment syntax, so it does nothing
  there. Turning it off in JSON leaves segments and nothing else.

All three are on by default, and they mirror `voxtype meeting export`'s own
flags.

### Where the file goes

The **Save to** field is pre-filled with your downloads directory (falling
back to your home directory) and the conversation's title as the filename, with
path separators and control characters replaced by `-`. It is an editable text
field — there is no file chooser — and a leading `~/` is expanded the way a
shell would. Nothing else is expanded: this is a path field, not a shell.

If a file is already there, the first **Export** click does not overwrite it.
Instead the dialog stays open, names the file, and the button becomes
**Replace?**; the next click on that same path goes through. Editing the path
cancels the pending confirmation, so confirming one path can never authorise
overwriting another.

Missing parent directories are created — but only once the destination is
settled, so a typo you then corrected leaves no empty directories behind.

On success you get "Exported to /home/you/Downloads/Standup.txt". A
conversation with no transcript is refused with "That conversation has no
transcript to export."

Lines that hold nothing that was said are skipped by every format.

## 12. Importing past voxtype meetings

**Settings → Import → Import past voxtype meetings** copies meetings recorded
by voxtype's own meeting mode into fastcription's library.

It reads them through voxtype itself — one `voxtype meeting list` for the
inventory, then `voxtype meeting export --format json` per meeting. voxtype's
own database and transcript files are never written to or modified. The listing
asks for every meeting explicitly, because voxtype's own default is the ten
most recent and taking that default silently dropped everything older.

What comes across: the meeting's title, its start time, its full transcript
with each segment's start and end in milliseconds, and any speaker labels
diarisation produced. Blank segments are dropped. The end time is computed
from the last segment's end. Each imported conversation remembers its voxtype
meeting id.

**Meetings still in progress are skipped**, along with paused and cancelled
ones — only completed meetings have a transcript to copy.

**It is safe to run twice.** Meetings already in the library are recognised by
their voxtype id and skipped, and each import is one transaction, so a failure
halfway leaves nothing behind to confuse the next attempt.

The import runs on a background thread, because each meeting costs a `voxtype`
subprocess of its own and a long history is tens of seconds. While it runs the
button is disabled and a spinner shows `N imported so far`. At the end you get
one notice:

> Imported 7 meeting(s), skipped 2.

and, if anything failed, the same line plus
`1 could not be imported: Standup: the meeting has no transcript`, raised as a
warning instead.

### Limitations

Imported conversations carry **no source and no model**, because voxtype's
export does not record them: its metadata has the id, title, times, status and
chunk count, and nothing about which device or which model produced the
transcript. Rather than invent values, fastcription writes the source as
`Imported from voxtype` and the engine as `voxtype (unknown)`. Both tracks
collapse into one, too: voxtype attributes segments by audio source rather than
by track, and the distinction is not in the export.

One more: when the JSON export has no start time of its own, fastcription falls
back to the `Date:` line from `voxtype meeting list`, which has no timezone and
is read as UTC — which is what voxtype writes there.

## 13. The transcription server

fastcription can send every transcription pass to a server instead of running
the model on this machine. The usual reason is hardware: a desktop with a GPU
can serve a laptop that has none, and because the model stays resident there,
no pass pays to load it. Set it up under **Settings → Transcription server**.

[SERVER.md](SERVER.md) walks the whole setup: installing CUDA, building
whisper.cpp for your card, which model to download and from where, running the
server, reaching it over Tailscale or through the firewall, every field in this
pane, and what to look at when something is wrong.

Two things to know here:

- **Test connection** transcribes half a second of silence through the server
  you configured, using the real path — the endpoint, the multipart body and
  the token — so a wrong address or a missing key is found now and not during a
  meeting. It runs on a background thread and shows a spinner and
  "asking the server…", because the interesting failure is an address nothing
  answers at, which takes the whole configured timeout to discover. The result
  appears beside the button as "Reached the server in 240ms" or as the reason
  it failed. Before you have run it, it reads "not tested".
- **The API key is kept for this session only.** It is written into
  fastcription's own voxtype config file, which is created readable by you
  alone, and never into the settings file that remembers everything else. The
  address, the model and the timeout do persist; the key you re-enter.

An `http://` address that is not on this machine draws a warning under the
fields: the audio crosses the network in the clear.

## 14. The voxtype service controls

**Settings → voxtype service** shows whether the daemon is `not installed`,
`running` or `stopped`, with **Start** and **Stop** buttons. They run
`systemctl --user start voxtype.service` and `systemctl --user stop
voxtype.service`, then re-read the real state rather than assuming the action
worked.

**This has nothing to do with fastcription's own transcription**, which runs
`voxtype transcribe` per pass and needs no daemon at all. As the pane says:

> fastcription transcribes without the daemon; the service is what provides
> voxtype's own push-to-talk dictation.

So the controls are here for your dictation setup, and the pill in the status
bar is there to tell you whether dictation works. A daemon started by hand
counts as running even when `systemctl` reports the unit inactive or absent,
because the question the pill answers is whether the daemon is there.

## 15. Notices

Everything the app tells you appears on one line above the transcript, newest
first, at one of three levels:

- **Info** — something worked, drawn in the accent colour. Dismisses itself
  after **6 seconds**.
- **Warning** — something is degraded and the app carries on, drawn in the
  warning colour. Stays until dismissed.
- **Error** — something you have to act on, drawn in the danger colour. Stays
  until dismissed.

The same text arriving again in a row does not stack up: it is counted, and the
line reads `transcribe: … ×12`. A transcriber failing every second otherwise
read as one problem flickering rather than as the same problem fifty times.

**Dismiss** takes the top notice away. When there are more, a **N more** button
appears; clicking it opens the rest beneath, newest first, with **Clear all**
at the bottom. At most 50 notices are kept — the list is for reading what just
went wrong, not for auditing a long session.

Startup reports every problem it found, not just the first: a machine with no
sound server *and* no voxtype has two things wrong with it.

## 16. The tray

fastcription keeps a tray icon with two entries: **Show fastcription** and
**Quit**.

- **Closing the window hides it to the tray** rather than quitting, as long as
  the tray exists. The session keeps running and keeps transcribing.
- **Show fastcription**, or clicking the icon, brings the window back when it is
  hidden and puts it away when it is up.
- **Quit** really quits. So does closing the window on a desktop with no tray.
- **Launching fastcription a second time** does not start a second copy: the
  running one raises and focuses its window instead.

Remember that compact mode is the same window, so hiding to the tray hides
your captions.

## 17. Keyboard shortcuts

| Key | What it does |
| --- | --- |
| `Ctrl+R` | Start a conversation, or resume a paused one |
| `Ctrl+Space` | Pause while recording, resume while paused |
| `Ctrl+.` | Stop |
| `Ctrl+M` | Enter or leave compact mode |
| `Ctrl+,` | Open Settings, or go back to the transcript |
| `Esc` | Leave compact mode |
| `Ctrl+=` or `Ctrl++` | Transcript text two points larger |
| `Ctrl+-` | Transcript text two points smaller |
| `Ctrl+0` | Transcript text back to 22 points |

Notes on the ones with conditions:

- **Nothing fires while a text field has the keyboard.** The app is full of
  them — a conversation title, a server address, a new tag — and `Ctrl+R`
  starting a recording mid-sentence instead of selecting a word is the kind of
  surprise that costs a meeting.
- **Nothing fires while a session is Finishing.** Every one of the transport
  keys is inert there.
- `Ctrl+R` does nothing while already recording; `Ctrl+Space` does nothing
  while idle; `Ctrl+.` does nothing unless there is something to stop.
- `Esc` only works while the window has keyboard focus. The recommended
  compositor rule for compact mode deliberately withholds focus so your meeting
  keeps it — which is exactly why compact mode draws its own **Restore**
  button.

## 18. Troubleshooting

### "voxtype was not found on PATH, so nothing can be transcribed."

fastcription looked on `PATH` and at `/usr/bin/voxtype` and found nothing.
Install voxtype, then run `voxtype setup --download` to fetch a model. The
readiness row's **Copy command** button gives you that command. Recording is
refused up front rather than per pass, because a session that records happily
and fails every transcription looks like a working app that understands
nothing.

### "No transcription model is installed."

voxtype is there, but `voxtype info models` reports nothing installed and no
model name is set in Settings. Run `voxtype setup model` (**Copy command**).
If you know the model is installed and the probe is simply failing, typing its
name into **Settings → Transcription → Model** is enough: the check passes on a
configured name, and the field becomes free text when voxtype did not report
its catalogue.

### "No sound server answered, so there is nothing to record from."

`pactl` returned nothing, or is not installed. Check PipeWire with
`systemctl --user status pipewire wireplumber` (**Copy command**).

A related warning, "No audio sources could be listed: …", means `pactl` ran
and failed, and the text after the colon is the reason. If it ends with
"did not answer within 5s; the sound server may be restarting", the sound
server is not responding at all rather than reporting an empty list.

### "The conversation library … will not accept writes"

The library file or its directory is read-only. Recording and importing are
disabled; **past transcripts are still readable, searchable and exportable**.
The readiness row's **Copy path** gives you the path to look at — check the
permissions of `~/.local/share/fastcription/library.db` and of the directory
holding it.

A different message, "The conversation library could not be opened, so
recording is disabled: …", means the file could not be opened at all. If it
says the schema is newer than this build knows, the library was written by a
newer fastcription and opening it would risk the transcripts; use the newer
build.

### The transcription server cannot be reached

**Test connection** reports the last line voxtype printed. Common causes:

- Nothing listening at that address, which takes the full **timeout s** (30
  seconds by default) to discover.
- The server serves the wrong path. voxtype posts to
  `{address}/v1/audio/transcriptions`; `whisper-server` serves `/inference`
  unless you pass `--inference-path`. See [SERVER.md](SERVER.md).
- A missing or wrong **API key**. Remember it is not remembered across
  launches.

During a meeting a server that goes away is not fatal. The sentence being
spoken stays buffered and the pass is retried, so a restart or a moment of bad
wireless costs only a pause in the transcript. After three failed attempts to
finish that one sentence, fastcription keeps whatever words had already been
agreed, reports the error and starts the next sentence clean — a server that
has gone for good cannot pin audio in memory or make every retry upload a
larger recording.

### "voxtype does not know the model '…' and silently fell back to its own default"

voxtype accepts an unknown model name, transcribes with its own default and
exits successfully, which would make the engine recorded with your conversation
a lie. fastcription treats it as an error instead. Pick a model from the
dropdown, or check `voxtype info models`.

### "transcription behind — words arrive late"

One transcription pass is taking longer than the **seconds between passes**
setting. **No audio is dropped** — captured audio waits in memory rather than
being discarded, and the interval stretches to at least the length of the last
pass so the backlog cannot grow without bound. The cost is latency, not
transcript.

If it persists, in rough order of effect:

- Check **Fast mode** is on. It is the single biggest difference for short
  passes.
- Use a smaller model. `base.en` is the default for this reason.
- Raise **seconds between passes**. Fewer passes is less repeated work, not
  less transcript.
- Move the model to another machine — section 13.

### "Audio source lost: …" / "The audio source is back."

The capture ended and fastcription is retrying. It re-resolves the source every
attempt, so an application that restarted is picked up again under its new
stream index. The warning is taken away by itself when audio returns, and the
transcript continues. If it never returns, the reason in the message is
`parec`'s own last words.

### "… matches 2 streams right now (…); fastcription will not guess which one to record"

Two live streams fit the application you chose — a browser with a call in one
tab and music in another is exactly this. fastcription keeps retrying rather
than recording a coin flip. Close or rename one of them, or stop the session
and pick the specific stream from the refreshed picker, where each candidate is
listed with its own stream title.

### "… is not available: …" when pressing Start

The source you had selected could not be resolved right now. Press the refresh
button beside the picker and choose again. This is checked at Start rather than
trusted, because an application stream's index goes stale when the application
restarts, and recording the wrong stream is worse than refusing.

### "voxtype's stdout did not look like the expected banner … shape"

voxtype printed something fastcription does not recognise. Its command-line
output is not a versioned interface, so a parse failure is reported as an error
rather than quietly treated as a silent passage — a silent transcriber is
indistinguishable from a quiet room, which is the worst way this app can fail.
Check that `voxtype -q transcribe <file>` works by hand, and which voxtype
version you have.

### Words sometimes repeat

Turn **Fast mode** off. It is voxtype's context-window optimisation, and that
is the one artefact to watch for.

### A word now and then is wrong, and never corrected

By design. A word is published once two consecutive passes agree on it and is
never revised after that, which is what keeps the transcript from flickering
while you read it. The trade is that an occasional word commits early and
wrong.

### Compact mode is covered by the meeting window

winit cannot ask a Wayland compositor to keep a window above a fullscreened
one; "always on top" is a hint. Install the one window rule from
[OVERLAY.md](OVERLAY.md).

### Something crashed

A panic is written to `~/.local/share/fastcription/panic.log`. For more detail
in the terminal, run with `RUST_LOG=debug`.
