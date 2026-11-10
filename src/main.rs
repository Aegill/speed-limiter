// В релизной сборке под Windows не показываем консольное окно.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod config;
mod engine;
mod model;
mod packet;
mod shaper;
mod theme;

fn main() -> eframe::Result {
    let shared = model::Shared::new();
    model::spawn_refresher(shared.clone());

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Лимит скорости")
            .with_inner_size([780.0, 660.0])
            .with_min_inner_size([640.0, 420.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Лимит скорости",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, shared)))),
    )
}
