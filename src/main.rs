// Hide console window on Windows release builds
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod processing;
mod state;
mod theme;
mod worker;

use eframe::egui;

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("ComicInfo Generator")
            .with_inner_size([1060.0, 760.0])
            .with_min_inner_size([860.0, 620.0]),
        ..Default::default()
    };

    eframe::run_native(
        "ComicInfo Generator",
        native_options,
        Box::new(|cc| Ok(Box::new(app::ComicInfoApp::new(cc)))),
    )
}
