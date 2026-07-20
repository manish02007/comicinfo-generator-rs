// Hide console window on Windows release builds
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod processing;
mod state;
mod theme;
mod worker;

use eframe::egui;

// App icon, embedded directly in the binary so no external file needs to
// ship alongside the executable (matters most for the plain .tar.gz /
// manual-install case, which has no installer to place a separate asset).
// 256x256 balances a crisp taskbar/alt-tab icon against binary size --
// egui/the OS compositor downscales it for smaller contexts (dock, tray).
const ICON_PNG: &[u8] = include_bytes!("../assets/icon_256.png");

/// Decodes the embedded PNG into the raw RGBA egui/eframe needs for a
/// window icon. Falls back to no icon (rather than panicking) if the
/// embedded bytes are ever somehow corrupt -- a missing icon is a cosmetic
/// issue, not worth crashing the whole app over.
fn load_icon() -> Option<egui::IconData> {
    let image = image::load_from_memory(ICON_PNG).ok()?.into_rgba8();
    let (width, height) = image.dimensions();
    Some(egui::IconData { rgba: image.into_raw(), width, height })
}

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

    // app_id matches the .desktop file's filename (and its Name= /
    // StartupWMClass, in turn matching this binary's name) -- Wayland's
    // compositor uses this to associate the running window with its
    // .desktop entry for the taskbar/dock/launcher. X11's equivalent,
    // WM_CLASS, already defaults to the binary name with no extra
    // wiring needed, but Wayland's app_id has no such automatic fallback.
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("ComicInfo Generator")
        .with_app_id("comicinfo-generator")
        .with_inner_size([1060.0, 760.0])
        .with_min_inner_size([860.0, 620.0]);
    if let Some(icon) = load_icon() {
        viewport = viewport.with_icon(icon);
    }

    let native_options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "ComicInfo Generator",
        native_options,
        Box::new(|cc| Ok(Box::new(app::ComicInfoApp::new(cc)))),
    )
}