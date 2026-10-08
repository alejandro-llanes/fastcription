//! fastframe wiring: logging, the single-instance guard, the tray-aware
//! shell, and eframe's own window bootstrap. Everything product-shaped lives
//! in `app`; this file only gets a window open.

mod app;
mod compositor;
mod env;
mod i18n;
mod icons;
mod session;
mod theme;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use app::App;

fn main() -> anyhow::Result<()> {
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

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
