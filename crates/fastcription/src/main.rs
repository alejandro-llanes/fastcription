//! fastframe wiring: logging, the single-instance guard, the tray-aware
//! shell, and eframe's own window bootstrap. Everything product-shaped lives
//! in `app`; this file only gets a window open.

mod app;
mod compositor;
mod env;
mod i18n;
mod icons;
mod logging;
mod lookup;
mod session;
mod theme;
mod ui;
mod visualizer;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use app::App;

fn main() -> anyhow::Result<()> {
    // Answered before anything else is set up. A GUI binary that has just been
    // put on someone's PATH by a shell script is going to be asked what it is
    // from a terminal, and opening a window — or failing to, on a machine with
    // no display — is the wrong answer to `--version`.
    if let Some(reply) = cli_reply(std::env::args().skip(1)) {
        println!("{reply}");
        return Ok(());
    }

    init_logging();

    // Built before the claim, because the claim's handler needs it: a second
    // launch hands "show" to this process, and the only way to act on it is to
    // leave a flag and wake whichever window exists. With the waker created
    // afterwards, the handler had nothing to wake and the second launch
    // exited silently, which looked exactly like the app being dead.
    let shell_waker = fastframe_shell::Waker::default();
    let show_requested = Arc::new(AtomicBool::new(false));

    let slot = fastframe_instance::Slot::new("dev.fastcription.App");
    let guard = {
        let waker = shell_waker.clone();
        let flag = Arc::clone(&show_requested);
        match slot.claim("show", move |_request| {
            flag.store(true, Ordering::SeqCst);
            waker.wake();
            Some("ok".to_owned())
        }) {
            fastframe_instance::Claim::First(guard) => guard,
            fastframe_instance::Claim::Running(_)
            | fastframe_instance::Claim::Declined
            | fastframe_instance::Claim::Unanswered => {
                tracing::info!("fastcription is already running");
                return Ok(());
            }
        }
    };

    let app = App::new(&shell_waker, show_requested);
    let mut restored = false;

    fastframe_shell::Shell::new(app, &shell_waker)
        .idle(fastframe_tray::idle)
        .run(|lease| {
            let restore = !std::mem::replace(&mut restored, true);
            eframe::run_native(
                "fastcription",
                native_options(),
                Box::new(move |cc| {
                    let mut app = lease.take(&cc.egui_ctx);
                    // Only for the first window. A window reopened from the
                    // tray must not have the live state replaced by whatever
                    // was last written to disk.
                    if restore {
                        if let Some(storage) = cc.storage {
                            app.restore(storage);
                        }
                    }
                    app.attach(&cc.egui_ctx);
                    Ok(Box::new(Window { app }) as Box<dyn eframe::App>)
                }),
            )
        })
        .map_err(|error| anyhow::anyhow!("eframe failed: {error}"))?;

    drop(guard);
    Ok(())
}

struct Window {
    app: fastframe_shell::Held<App>,
}

impl eframe::App for Window {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.app.frame(ui, frame);
    }

    /// eframe calls this on exit and every half minute. Nothing in the app was
    /// persisted before it existed: every launch reset the engine, the model,
    /// the server address and the chosen source.
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.app.save(storage);
    }
}

/// What to print and exit for, if anything.
///
/// Deliberately tiny: fastcription is configured in its own window, not on a
/// command line, so there are no options to parse — only the two questions a
/// terminal asks of any binary. An unrecognised argument is reported rather
/// than ignored, because silently opening the window is how a typo in a
/// desktop entry goes unnoticed.
fn cli_reply(args: impl Iterator<Item = String>) -> Option<String> {
    const USAGE: &str = concat!(
        "fastcription ",
        env!("CARGO_PKG_VERSION"),
        "\n\n\
         Realtime conversation transcription for the Linux desktop.\n\n\
         Usage: fastcription [--version] [--help]\n\n\
         Everything else is configured in the window, under Settings.\n\
         Set RUST_LOG to override the log level for one run.\n\n\
         Documentation: https://github.com/alejandro-llanes/fastcription"
    );
    let mut unknown = Vec::new();
    for arg in args {
        match arg.as_str() {
            "-V" | "--version" => {
                return Some(format!("fastcription {}", env!("CARGO_PKG_VERSION")))
            }
            "-h" | "--help" => return Some(USAGE.to_owned()),
            other => unknown.push(other.to_owned()),
        }
    }
    if unknown.is_empty() {
        return None;
    }
    Some(format!(
        "fastcription takes no arguments; got {}.\n\n{USAGE}",
        unknown.join(" ")
    ))
}

fn native_options() -> eframe::NativeOptions {
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("fastcription")
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([760.0, 480.0]),
        ..Default::default()
    }
}

fn init_logging() {
    let log_dir = dirs::data_dir()
        .map(|dir| dir.join("fastcription"))
        .unwrap_or_else(std::env::temp_dir);
    fastframe_log::log_panics(
        log_dir.join("panic.log"),
        "fastcription",
        env!("CARGO_PKG_VERSION"),
    );

    // Behind a reload handle, so the level in Settings takes effect at once.
    // See `logging.rs` for why the default is `warn` and why `RUST_LOG` wins.
    {
        use tracing_subscriber::layer::SubscriberExt as _;
        use tracing_subscriber::util::SubscriberInitExt as _;
        let (filter, handle) = tracing_subscriber::reload::Layer::new(logging::initial_filter());
        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer())
            .init();
        logging::install(handle);
    }
}

#[cfg(test)]
mod tests {
    use super::cli_reply;

    fn reply(args: &[&str]) -> Option<String> {
        cli_reply(args.iter().map(|a| (*a).to_owned()))
    }

    /// No arguments means open the window, which is the whole point of the
    /// program; anything printed instead would be a window that never opened.
    #[test]
    fn no_arguments_opens_the_window() {
        assert_eq!(reply(&[]), None);
    }

    #[test]
    fn version_and_help_are_answered_in_both_spellings() {
        let version = format!("fastcription {}", env!("CARGO_PKG_VERSION"));
        assert_eq!(reply(&["--version"]).as_deref(), Some(version.as_str()));
        assert_eq!(reply(&["-V"]).as_deref(), Some(version.as_str()));
        for help in [reply(&["--help"]), reply(&["-h"])] {
            let help = help.expect("help is answered");
            assert!(help.contains("Usage: fastcription"), "{help}");
            assert!(help.contains(env!("CARGO_PKG_VERSION")), "{help}");
        }
    }

    /// A typo in a desktop entry or a shell alias should say so rather than
    /// open a window as though nothing had been asked.
    #[test]
    fn an_unknown_argument_is_reported_with_the_usage() {
        let reply = reply(&["--transcribe-everything"]).expect("reported");
        assert!(reply.contains("--transcribe-everything"), "{reply}");
        assert!(reply.contains("Usage: fastcription"), "{reply}");
    }
}
