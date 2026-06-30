// Hide console window on Windows release builds
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod processing;
mod state;
mod theme;
mod worker;

use eframe::egui;

/// Writes panic details (message + source location + timestamp) to
/// logs/crash.log before falling through to the default handler, so a
/// crash leaves a paper trail on disk instead of vanishing the moment the
/// window closes -- there's currently no other way to see what happened
/// short of reproducing it live in a terminal.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let log_dir = std::env::current_dir().unwrap_or_default().join("logs");
        let _ = std::fs::create_dir_all(&log_dir);
        let crash_log = log_dir.join("crash.log");
        let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");

        let location = info.location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown location".to_string());
        let payload = info.payload().downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "(non-string panic payload)".to_string());

        let entry = format!(
            "[{ts}] PANIC at {location}\n  {payload}\n\n"
        );
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&crash_log) {
            use std::io::Write;
            let _ = f.write_all(entry.as_bytes());
        }

        // Still run the default handler so stderr/terminal output (and
        // debug-build RUST_BACKTRACE behavior) is unaffected.
        default_hook(info);
    }));
}

fn main() -> eframe::Result<()> {
    install_panic_hook();

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