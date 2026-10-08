# Compact mode: captions over a meeting window

ARCHITECTURE.md decision D3 asks for captions that stay visible over a
fullscreened meeting window. fastcription provides them as **compact mode**: the
main window shrinks to a caption strip, drops its decorations, asks to be
always on top, and renames itself to `fastcription — captions`. `Ctrl+M`
toggles it, the top bar has a **Compact** button, and `Esc` or the strip's own
**Restore** button bring the full window back.

> **Why not a second window.** This was built first as a second, undecorated,
> always-on-top egui window — a *deferred viewport*. It never worked. The app's
> repaint requests target the root viewport, so the captions froze after their
> first frame; the sync only ran when a sentence was committed, so even
> repainting it would have been a whole sentence behind; and a deferred viewport
> is a child of the root, so hiding the main window to the tray destroyed it.
> One window that changes shape has none of those problems.
>
> **The trade.** Compact mode *is* the main window. Hiding fastcription to the
> tray hides the captions with it, by design — there is one window, and the tray
> is how you put it away. Stop compact mode before hiding, or leave the window
> up.

## What the strip shows

The last four committed lines, then the line being spoken, in a window 760 ×
170 points. The line being spoken is drawn in italics, in the palette's
`secondary`, because its tail is replaced on every transcription pass and the
reader has to be able to tell settled words from unsettled ones. It used to be
drawn in `dim`, which on a light palette measured 3:1 against the panel — below
WCAG AA for text, for the one audience this application has. Before anything
has been said the strip reads "Waiting for speech…".

Along the foot: a record button, the recording clock, the audio visualiser,
and the **Restore** button on the right.

The record button is the one control besides Restore, and it is a toggle —
start, pause, resume. It pauses rather than stops because a toggle has to be
reversible: pressing it twice has to leave one conversation with a gap in it,
not two conversations. `Ctrl+.` still stops. It is filled with the accent
colour while recording, so the strip answers "is this transcribing?" without
anyone having to read the clock. It exists because reaching the transport from
compact mode previously meant restoring the window, pressing a button and
shrinking again — three actions and a window resize to stop transcribing a
coffee break.

The visualiser is the spectrum of the selected source, in whichever style is
set under **Settings → Appearance**; `Off` is one of them. In a strip that is
otherwise all text it is the quickest answer to "is this thing still hearing
the call?" when the captions have not moved for twenty seconds.

The size is the **transcript text size** setting, shared with the main
window's live transcript rather than being compact mode's own. It ranges from
14 to 40 points, defaults to 22, is persisted, and has three ways to change it:
the slider in the Live pane header, the slider in Settings, and `Ctrl+=` /
`Ctrl+-` / `Ctrl+0` (back to 22).

Compact mode answers every shortcut the full window does — the size keys above,
`Ctrl+R` / `Ctrl+Space` / `Ctrl+.` for start, pause and stop, and `Esc` to come
back. All of them need keyboard focus, which the recommended Hyprland rule
below deliberately withholds so the meeting keeps it; set the size before
entering compact mode, or use the **Restore** button and the full window's
controls. Nothing else in the strip is a control.

## The title is compact mode's, while it is on

The main window retitles itself as the session changes state: `● REC 12:34 —
fastcription` while recording, `⏸ Paused — fastcription` when paused,
`Finishing — fastcription` while the last of the audio is being transcribed,
plain `fastcription` when idle. **None of that happens while compact mode is
on.** The
title is the compositor rule's only handle on this window, and a title that
gained a `REC` prefix would float the captions for exactly as long as it took
the clock to tick over. Leaving compact mode restores whichever state title is
current.

winit has no layer-shell backend: on Wayland, "always on top" is an
`xdg_toplevel` hint the compositor is free to ignore, not a protocol guarantee
the way `zwlr_layer_shell_v1` would be. Worse, egui cannot resize its own
window on Wayland at all — measured against this workspace's egui revision, a
window opens at the size it asks for and every later resize request is ignored,
floating or tiled. So compact mode cannot shrink itself.

## Hyprland: nothing to configure

On Hyprland, fastcription asks the compositor directly, through `hyprctl`, and
no window rule is needed. Entering compact mode floats the window, resizes it
to the caption bar and pins it to every workspace; leaving puts back whatever
was there before, including leaving the window floating if that is how you
already had it.

A window rule cannot do this job, which is worth recording because it is the
obvious thing to try:

- **A rule matched on the compact title does nothing.** Hyprland evaluates
  window rules when a window maps and does not re-evaluate them when the title
  changes, so neither `float` nor `size` ever fires for a window that is
  already open. Verified with the rule installed before the window mapped.
- **A rule matched on the class floats the window**, because the class is known
  at map time — but a floating window still ignores the application's own
  resize requests, so the captions fill the whole window.

If you want the main window floating as well, that part is still yours to
configure, and it does work on the class:

```lua
hl.window_rule({
    name = "fastcription",
    match = { class = "^fastcription$" },
    float = true,
})
```

Hyprland 0.56 configures in Lua; older releases take
`windowrule = float, class:^(fastcription)$`. fastcription tries the Lua
dispatcher first and falls back to the older `hyprctl dispatch` syntax, so one
binary serves both — though only the Lua path has been exercised here.

## sway / river

fastcription only drives Hyprland. Everywhere else compact mode changes what is
drawn and leaves the window alone, so the caption bar is whatever size the
window already is, and these rules are how to give it a shape.

Whether a title-matched rule applies to a window that is already open is a
compositor's own business, and neither of these was tested here — if the rule
appears to do nothing, that is why, and matching on `app_id` instead will at
least apply from the moment the window opens.

Neither has `pin`; the equivalent is marking the window for every workspace and
keeping it floating.

**sway** (`~/.config/sway/config`) — sway matches titles with `title="…"`:

```
for_window [title="^fastcription — captions$"] floating enable
for_window [title="^fastcription — captions$"] sticky enable
for_window [title="^fastcription — captions$"] border none
```

`sticky` is sway's per-output "show on every workspace of this output"
equivalent to Hyprland's `pin`; it does not cross outputs, and like Hyprland's
`pin` it does not force the window above a fullscreened one — sway, like
Hyprland, leaves "always on top" as a hint a client makes, not a rule a
compositor enforces, so a fullscreened meeting can still cover it.

**river** has no declarative window-rule config file, and its `rule-add` matches
on app id and title:

```sh
riverctl rule-add -title 'fastcription — captions' float
```

river has no sticky/pin equivalent at all: a window stays only on the tags it
was mapped with. Since compact mode is the same window as the full one, it keeps
whatever tags fastcription was opened on — which in practice is the tag the user
is working on anyway.

## Fallback

With no rule installed, compact mode still works: a small, undecorated,
floating window showing the captions, which most compositors will honour the
always-on-top hint for against ordinary windows and will not against a
fullscreened one. Nothing in the app depends on the rule existing — the rule
only buys the fullscreen case.
