# ComicInfo Generator — Rust Edition

A fast, modern GUI application for generating and embedding `ComicInfo.xml`
metadata into CBZ comic archive files.  Full port of the Python/tkinter version
to Rust + egui, with every original feature preserved.

---

## Features

| Feature | Description |
|---------|-------------|
| **Batch CBZ processing** | Embeds `ComicInfo.xml` into every `.cbz` in a folder |
| **Parallel processing** | Rayon thread-pool (configurable worker count) |
| **Smart renaming** | `Episode 1 - Title.cbz` format with zero-padding option |
| **Prefix modes** | Auto / Episode / Chapter / Volume / Custom |
| **Volume metadata** | Chapter→Volume rules, per-volume date & summary rules |
| **Decimal chapters** | GUI dialog for bonus/extra chapter labelling |
| **Final chapter** | Optional `"Final Chapter: …"` title formatting |
| **Post-finale strips** | Skip prefix for side-stories after finale |
| **Session resume** | Progress log so interrupted runs can continue |
| **Dry-run mode** | Preview all changes without touching any file |
| **Config save/load** | JSON configs with smart filename suggestion |
| **Import metadata** | Load from `.json` or `.py` metadata files |
| **Autosave** | Session restored automatically on next launch |
| **Custom XML fields** | Any extra `<Tag>Value</Tag>` in ComicInfo.xml |
| **Keyboard shortcuts** | Ctrl+S / O / I / R |

---

## Building

### Requirements

| Tool | Minimum version |
|------|----------------|
| Rust + Cargo | **1.80** (install via [rustup.rs](https://rustup.rs)) |
| On Linux | `libgtk-3-dev`, `pkg-config`, and either X11 or Wayland |

### Linux dependencies

```bash
# Debian / Ubuntu
sudo apt install libgtk-3-dev pkg-config build-essential

# Fedora / RHEL
sudo dnf install gtk3-devel pkg-config gcc

# Arch
sudo pacman -S gtk3 pkg-config base-devel
```

### Build

```bash
# Debug (fast compile, larger binary)
cargo run

# Release (optimised, small binary, no console on Windows)
cargo build --release
# Binary at: target/release/comicinfo-generator
```

### Cross-compile for Windows (from Linux)

```bash
rustup target add x86_64-pc-windows-gnu
sudo apt install mingw-w64
cargo build --release --target x86_64-pc-windows-gnu
```

---

## Cross-platform notes

| OS | Backend |
|----|---------|
| **Windows** | Win32 via winit — console hidden in release builds |
| **macOS** | Cocoa via winit |
| **Linux X11** | OpenGL via glutin (eframe glow renderer) |
| **Linux Wayland** | OpenGL via EGL; XWayland also works automatically |
| **Any DE** | No DE-specific dependencies; pure GTK for file dialogs only |

The app sets `WINIT_UNIX_BACKEND=wayland` or `=x11` automatically via
winit's runtime detection.  If your Wayland session lacks XDG portal support
the file picker falls back gracefully to a basic path entry field.

---

## Usage

### Workflow

1. **Paths & Config tab** — point at your CBZ folder and optional JSON title/date files
2. **Processing tab** — set mode (Manga/Manhwa), prefix style, separators, zero-padding
3. **Metadata tab** — fill in constant ComicInfo fields (Series, Writer, Publisher …)
4. **Rules tab** — define chapter→volume mappings, per-volume publication dates/summaries
5. **Run tab** — hit ▶ Start; watch the live log; stats appear at the bottom

### JSON title files

```json
{
  "1":   "Rise of the Hero",
  "2":   "The Dark Forest",
  "2.5": "Bonus: Side Story"
}
```

Keys are chapter/volume numbers as strings (decimal OK).

### Config files

Save your full setup via **💾 Save Config** (Ctrl+S).  The filename is
auto-suggested from your folder/series name.  Reload with **📂 Load Config** (Ctrl+O).

### Autosave

Settings are saved to `~/.comicinfo_autosave.json` every 30 seconds and on exit.

---

## Project layout

```
src/
  main.rs        — entry point, window setup
  app.rs         — full egui UI: tabs, dialogs, toolbar
  worker.rs      — background processing thread
  processing.rs  — CBZ/XML logic, filename parsing, rule lookups
  state.rs       — AppConfig, RunStats, LogEntry types
  theme.rs       — color palette, egui style setup
```

---

## License

MIT — do whatever you want with it.
