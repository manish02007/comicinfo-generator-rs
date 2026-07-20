// Embeds the app icon into the Windows .exe itself (Explorer, taskbar, and
// alt-tab all read this from the PE resource section, separate from the
// egui::IconData window icon set at runtime in main.rs -- that one only
// covers the running window, not the file icon Explorer shows before the
// app is even launched). No-op on every other target.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icons/icon.ico");
        if let Err(e) = res.compile() {
            // Don't hard-fail the whole build over a cosmetic resource --
            // print the reason and keep going with the default exe icon.
            println!("cargo:warning=failed to embed Windows icon: {e}");
        }
    }
}