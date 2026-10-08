//! Asking the compositor for compact mode's window geometry.
//!
//! egui cannot resize its own window on Wayland. Measured against this
//! workspace's egui revision with a throwaway probe: a window opens at the
//! size it asks for, and every later `ViewportCommand::InnerSize` is a no-op —
//! once or repeated every frame, floating or tiled, with or without a size
//! minimum in the way. So compact mode cannot shrink itself, and something
//! outside the process has to do it.
//!
//! On Hyprland that works, and the recipe was found by trying the
//! alternatives first:
//!
//! - A window rule matched on the compact title does nothing. Rules are
//!   evaluated when a window maps and are not re-evaluated when its title
//!   changes, so neither `float` nor `size` ever fires for a window that was
//!   already open. Verified with the rule installed before the window mapped.
//! - A window rule matched on the class does float the window, because the
//!   class is known at map time — but a floating window still ignores the
//!   application's own resize requests, so the captions filled the whole
//!   window.
//! - Floating the window and then asking the compositor to resize it gives
//!   exactly the requested geometry.
//!
//! Everything here is best effort, on its own thread. Another compositor, a
//! missing `hyprctl` or a failed request leaves compact mode drawing in
//! whatever window it already has, which is a smaller version of the feature
//! rather than a broken one.

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The class the window maps with, which is how the compositor is told which
/// window is meant. One instance runs per user, so it identifies exactly one.
pub const APP_CLASS: &str = "fastcription";

/// How long a `hyprctl` call may take. It answers in milliseconds; a wedged
/// compositor socket must not hold a thread for ever.
const TIMEOUT: Duration = Duration::from_secs(3);

/// How long to let the window's new size minimum reach the compositor before
/// asking for a size below the old one. Several frames at 60 Hz.
const MIN_SIZE_SETTLE: Duration = Duration::from_millis(120);

/// What the window looked like before compact mode took it over, so leaving
/// restores what the user had rather than an assumption. A user whose config
/// floats this window should not find it tiled afterwards.
static BEFORE: Mutex<Option<Geometry>> = Mutex::new(None);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Geometry {
    width: i64,
    height: i64,
    floating: bool,
    pinned: bool,
}

/// Shrinks the window to a caption bar: floating, the given size, and pinned
/// so the captions follow the user across workspaces.
pub fn enter_compact(size: [f32; 2]) {
    let width = size[0].round() as i64;
    let height = size[1].round() as i64;
    spawn("enter compact", move || {
        let before = window()?;
        *BEFORE.lock().unwrap_or_else(|e| e.into_inner()) = Some(before);
        // The float and pin dispatchers toggle, so the current state decides
        // whether to send them at all.
        if !before.floating {
            dispatch(Action::Float)?;
        }
        if !before.pinned {
            dispatch(Action::Pin)?;
        }
        // The caller lowers the window's own size minimum in the same frame
        // that this thread starts, and a compositor clamps a resize to that
        // minimum. The two reach the compositor by different routes, so wait
        // for the slower one rather than racing it.
        std::thread::sleep(MIN_SIZE_SETTLE);
        dispatch(Action::Resize { width, height })
    });
}

/// Puts back the geometry compact mode took over.
pub fn leave_compact() {
    let before = BEFORE.lock().unwrap_or_else(|e| e.into_inner()).take();
    spawn("leave compact", move || {
        let Some(before) = before else {
            // Nothing was recorded, so there is nothing trustworthy to put
            // back. Leaving the window as it is beats guessing a size.
            return Ok(());
        };
        let now = window()?;
        if before.floating {
            dispatch(Action::Resize {
                width: before.width,
                height: before.height,
            })?;
        }
        if now.pinned != before.pinned {
            dispatch(Action::Pin)?;
        }
        if now.floating != before.floating {
            dispatch(Action::Float)?;
        }
        Ok(())
    });
}

#[derive(Debug, Clone, Copy)]
enum Action {
    Float,
    Pin,
    Resize { width: i64, height: i64 },
}

fn spawn(what: &'static str, work: impl FnOnce() -> Result<(), String> + Send + 'static) {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none() {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("fc-compositor".into())
        .spawn(move || match work() {
            Ok(()) => tracing::debug!(what, "compositor applied the compact geometry"),
            Err(err) => tracing::debug!(what, %err, "compositor request failed"),
        });
    if let Err(err) = spawned {
        tracing::debug!(what, %err, "could not spawn the compositor thread");
    }
}

/// This window as the compositor sees it.
fn window() -> Result<Geometry, String> {
    parse_clients(&run(&["clients", "-j"])?, APP_CLASS)
}

fn parse_clients(json: &str, class: &str) -> Result<Geometry, String> {
    let clients: serde_json::Value =
        serde_json::from_str(json).map_err(|err| format!("hyprctl clients: {err}"))?;
    clients
        .as_array()
        .into_iter()
        .flatten()
        .find(|client| client.get("class").and_then(|c| c.as_str()) == Some(class))
        .map(|client| {
            let size = client.get("size").and_then(|s| s.as_array());
            let at = |i: usize| {
                size.and_then(|s| s.get(i))
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0)
            };
            Geometry {
                width: at(0),
                height: at(1),
                floating: client
                    .get("floating")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                pinned: client
                    .get("pinned")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
            }
        })
        .ok_or_else(|| format!("no window of class {class}"))
}

/// Issues one dispatcher for this window.
///
/// Hyprland 0.56 configures in Lua and rejects the older `hyprctl dispatch`
/// argument syntax outright; releases before it only understand the older
/// form. The Lua form is tried first and the older one on any failure, so one
/// binary serves both.
fn dispatch(action: Action) -> Result<(), String> {
    match run(&["eval", &lua(action)]) {
        Ok(_) => Ok(()),
        Err(lua_err) => {
            let (dispatcher, arg) = legacy(action);
            run(&["dispatch", dispatcher, &arg])
                .map(|_| ())
                .map_err(|old_err| format!("lua: {lua_err}; legacy: {old_err}"))
        }
    }
}

/// Lua that finds this window by class and applies a `hl.dsp.window`
/// dispatcher to it. Selecting by class rather than by address, because the
/// address a Lua window object exposes is not documented to be the one
/// `hyprctl clients` prints.
fn lua(action: Action) -> String {
    let call = match action {
        Action::Float => "hl.dsp.window.float({ window = w })".to_owned(),
        Action::Pin => "hl.dsp.window.pin({ window = w })".to_owned(),
        Action::Resize { width, height } => format!(
            "hl.dsp.window.resize({{ window = w, exact = true, x = {width}, y = {height} }})"
        ),
    };
    format!(
        "local w for _, x in ipairs(hl.get_windows()) do if x.class == \"{APP_CLASS}\" then w = x end end \
         if not w then error(\"no window of class {APP_CLASS}\") end hl.dispatch({call})"
    )
}

/// The pre-Lua `hyprctl dispatch` form. Untested here: this machine runs a
/// Hyprland that only accepts Lua.
fn legacy(action: Action) -> (&'static str, String) {
    match action {
        Action::Float => ("togglefloating", format!("class:{APP_CLASS}")),
        Action::Pin => ("pin", format!("class:{APP_CLASS}")),
        Action::Resize { width, height } => (
            "resizewindowpixel",
            format!("exact {width} {height},class:{APP_CLASS}"),
        ),
    }
}

/// Runs `hyprctl` with a deadline, returning stdout. Hyprland reports Lua
/// errors on stdout with a zero exit status, so an `error` prefix counts as
/// failure too.
fn run(args: &[&str]) -> Result<String, String> {
    let mut child = Command::new("hyprctl")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("could not run hyprctl: {err}"))?;

    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = String::new();
                if let Some(mut stdout) = child.stdout.take() {
                    let _ = stdout.read_to_string(&mut out);
                }
                let mut err = String::new();
                if let Some(mut stderr) = child.stderr.take() {
                    let _ = stderr.read_to_string(&mut err);
                }
                if !status.success() {
                    return Err(format!("hyprctl {}: {}", args[0], err.trim()));
                }
                if out.trim_start().starts_with("error") || err.trim_start().starts_with("error") {
                    return Err(format!("hyprctl {}: {}{}", args[0], out.trim(), err.trim()));
                }
                return Ok(out);
            }
            Ok(None) if started.elapsed() > TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "hyprctl {} did not answer within {TIMEOUT:?}",
                    args[0]
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(err) => return Err(format!("hyprctl {}: {err}", args[0])),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLIENTS: &str = r#"[
        {"class":"brave-browser","size":[1896,1008],"floating":false,"pinned":false},
        {"class":"fastcription","size":[1180,760],"floating":true,"pinned":false},
        {"class":"hyprland-dialog","size":[550,147],"floating":true,"pinned":true}
    ]"#;

    #[test]
    fn reads_this_window_geometry_and_state() {
        assert_eq!(
            parse_clients(CLIENTS, "fastcription").expect("found"),
            Geometry {
                width: 1180,
                height: 760,
                floating: true,
                pinned: false
            }
        );
    }

    #[test]
    fn a_missing_window_is_an_error_not_a_guess() {
        assert!(parse_clients(CLIENTS, "nothing").is_err());
        assert!(parse_clients("not json", "fastcription").is_err());
        // A window without the fields still parses, with the defaults that
        // make the caller send a float and a pin rather than skip them.
        let bare = r#"[{"class":"fastcription"}]"#;
        let geometry = parse_clients(bare, "fastcription").expect("found");
        assert!(!geometry.floating && !geometry.pinned);
    }

    /// The exact Lua that was verified against Hyprland 0.56: selecting by
    /// class, and `exact` so the size is pixels rather than a delta.
    #[test]
    fn the_lua_matches_what_the_compositor_accepted() {
        let resize = lua(Action::Resize {
            width: 760,
            height: 170,
        });
        assert!(resize.contains("x.class == \"fastcription\""));
        assert!(
            resize.contains("hl.dsp.window.resize({ window = w, exact = true, x = 760, y = 170 })")
        );
        assert!(lua(Action::Float).contains("hl.dsp.window.float({ window = w })"));
        assert!(lua(Action::Pin).contains("hl.dsp.window.pin({ window = w })"));
    }

    #[test]
    fn the_legacy_form_names_the_old_dispatchers() {
        assert_eq!(legacy(Action::Float).0, "togglefloating");
        let (dispatcher, arg) = legacy(Action::Resize {
            width: 760,
            height: 170,
        });
        assert_eq!(dispatcher, "resizewindowpixel");
        assert_eq!(arg, "exact 760 170,class:fastcription");
    }
}
