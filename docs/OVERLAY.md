# The caption overlay on Wayland

ARCHITECTURE.md decision D3 gives fastcription a second, undecorated,
always-on-top window (`crates/fastcription/src/app/overlay.rs`) that shows the
last few committed lines in a large font, independent of the main window.

winit has no layer-shell backend: on Wayland, `with_always_on_top()` is a
`xdg_toplevel` hint (`set_always_on_top`-equivalent state) the compositor is
free to ignore, not a protocol guarantee the way `zwlr_layer_shell_v1` would
be. On this machine (Hyprland/Omarchy) that means the overlay needs a window
rule to actually float above a fullscreened meeting window; without one, a
focused call can still cover it.

The overlay sets its own Wayland `app_id`, `fastcription-overlay`, distinct
from the main window's, specifically so a compositor rule can target it alone.

## Hyprland

Add to `~/.config/hypr/hyprland.conf` (or a file it `source`s, as Omarchy's
`~/.config/hypr/windowrules.conf`):

```
windowrulev2 = float, class:^(fastcription-overlay)$
windowrulev2 = pin, class:^(fastcription-overlay)$
windowrulev2 = noborder, class:^(fastcription-overlay)$
windowrulev2 = noshadow, class:^(fastcription-overlay)$
windowrulev2 = stayfocused, class:^(fastcription-overlay)$, negative:true
```

- `float` takes it out of tiling, so it keeps the size and position the app
  requests.
- `pin` keeps it visible on every workspace, including over a fullscreened
  window — this is what `with_always_on_top()` cannot get on its own.
- `noborder`/`noshadow` match the overlay's own undecorated styling.
- The `stayfocused ... negative:true` rule stops Hyprland from ever handing
  the overlay keyboard focus, so a meeting app underneath keeps its own focus
  while the overlay floats on top of it.

Reload with `hyprctl reload`, or restart Hyprland.

## sway / river

Neither has `pin`; the equivalent is marking the window for every workspace
and keeping it floating:

**sway** (`~/.config/sway/config`):

```
for_window [app_id="fastcription-overlay"] floating enable
for_window [app_id="fastcription-overlay"] sticky enable
for_window [app_id="fastcription-overlay"] border none
```

`sticky` is sway's per-output "show on every workspace of this output"
equivalent to Hyprland's `pin`; it does not cross outputs, and like Hyprland's
`pin` it does not force the window above a fullscreened one — sway, like
Hyprland, leaves "always on top" as a hint a client makes, not a rule a
compositor enforces, so a fullscreened meeting can still cover it.

**river** has no declarative window-rule config file; the equivalent goes
through `riverctl` in whatever script launches the session, floating and
tagging the window so a layout never tiles it:

```sh
riverctl rule-add -app-id fastcription-overlay float
```

river has no sticky/pin equivalent at all: an overlay stays only on the tags
it was mapped with. Put it on every tag the user actually uses, or accept that
switching tags hides it.

## Fallback

If no rule is installed, the overlay still opens as a normal floating,
undecorated window — not pinned above a fullscreened call, but otherwise
usable (moved and resized like any window, visible on its own workspace).
Nothing in `overlay.rs` depends on the rule existing.
