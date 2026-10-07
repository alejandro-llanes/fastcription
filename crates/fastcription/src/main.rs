//! Proves the fastframe/eframe git pins resolve and a window opens before any
//! real UI is built on top.
fn main() -> eframe::Result<()> {
    eframe::run_ui_native(
        "fastcription",
        eframe::NativeOptions::default(),
        |ctx, _frame| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.label("fastcription");
            });
        },
    )
}
