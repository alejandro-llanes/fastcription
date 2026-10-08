# Compact mode: captions over a meeting window

ARCHITECTURE.md decision D3 asks for captions that stay visible over a
fullscreened meeting window. fastcription provides them as **compact mode**: the
main window shrinks to a caption strip, drops its decorations, asks to be
always on top, and renames itself to `fastcription — captions`. `Ctrl+Shift+C`
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

Along the foot: the recording clock on the left, the **Restore** button on the
right.

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
the way `zwlr_layer_shell_v1` would be. On this machine (Hyprland/Omarchy) that
means compact mode needs a window rule to actually float above a fullscreened
meeting window; without one, a focused call can still cover it.

The rule matches on the **window title**, `fastcription — captions`, because
that is what changes when compact mode is entered and reverts when it is left —
the `app_id` stays `fastcription` for both shapes, so a rule on the app id
would also apply to the full window. The em dash in the title is U+2014, with
an ordinary space on each side.

## Hyprland

Add to `~/.config/hypr/hyprland.conf` (or a file it `source`s, as Omarchy's
`~/.config/hypr/windowrules.conf`):

```
windowrulev2 = float, title:^(fastcription — captions)$
windowrulev2 = pin, title:^(fastcription — captions)$
windowrulev2 = noborder, title:^(fastcription — captions)$
windowrulev2 = noshadow, title:^(fastcription — captions)$
windowrulev2 = stayfocused, title:^(fastcription — captions)$, negative:true
```

- `float` takes it out of tiling, so it keeps the size the app requests.
- `pin` keeps it visible on every workspace, including over a fullscreened
  window — this is what the always-on-top hint cannot get on its own.
- `noborder`/`noshadow` match compact mode's own undecorated styling.
- The `stayfocused ... negative:true` rule stops Hyprland from ever handing the
  captions keyboard focus, so a meeting app underneath keeps its own focus while
  the captions float on top of it. Note the consequence: with focus never
  arriving, `Esc` cannot reach fastcription, which is why compact mode draws its
  own **Restore** button.

Reload with `hyprctl reload`, or restart Hyprland.

## sway / river

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
