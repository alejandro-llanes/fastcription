//! fastframe wiring: logging, the single-instance guard, the tray-aware
//! shell, and eframe's own window bootstrap. Everything product-shaped lives
//! in `app`; this file only gets a window open.

mod app;
mod env;
mod i18n;
mod icons;
mod session;
mod theme;

use app::App;

fn main() -> anyhow::Result<()> {
    init_logging();

    let slot = fastframe_instance::Slot::new("dev.fastcription.App");
    let guard = match slot.claim("show", |_request| Some("ok".to_owned())) {
        fastframe_instance::Claim::First(guard) => guard,
        fastframe_instance::Claim::Running(_)
        | fastframe_instance::Claim::Declined
        | fastframe_instance::Claim::Unanswered => {
            tracing::info!("fastcription is already running");
            return Ok(());
        }
    };

    let shell_waker = fastframe_shell::Waker::default();
    let app = App::new(&shell_waker);

    fastframe_shell::Shell::new(app, &shell_waker)
        .idle(fastframe_tray::idle)
        .run(|lease| {
            eframe::run_native(
                "fastcription",
                native_options(),
                Box::new(move |cc| {
                    let mut app = lease.take(&cc.egui_ctx);
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
