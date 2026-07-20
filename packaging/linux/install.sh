#!/bin/sh
# Installs ComicInfo Generator for the current user: copies the binary to
# ~/.local/bin, the icon into the user's hicolor icon theme, and the
# .desktop file into ~/.local/share/applications so it shows up as a real
# application (launcher, taskbar, alt-tab) rather than a bare executable.
# No root required -- everything goes under $HOME, per the XDG base dir spec.
set -e

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
BIN_DIR="$HOME/.local/bin"
ICON_DIR="$HOME/.local/share/icons/hicolor"
APPS_DIR="$HOME/.local/share/applications"

mkdir -p "$BIN_DIR" "$APPS_DIR"
cp "$SCRIPT_DIR/comicinfo-generator" "$BIN_DIR/comicinfo-generator"
chmod 755 "$BIN_DIR/comicinfo-generator"

for size in 16 32 48 64 128 256; do
    mkdir -p "$ICON_DIR/${size}x${size}/apps"
    cp "$SCRIPT_DIR/icons/icon_${size}.png" \
       "$ICON_DIR/${size}x${size}/apps/comicinfo-generator.png"
done
mkdir -p "$ICON_DIR/scalable/apps"
cp "$SCRIPT_DIR/icon.svg" "$ICON_DIR/scalable/apps/comicinfo-generator.svg"

cp "$SCRIPT_DIR/comicinfo-generator.desktop" "$APPS_DIR/comicinfo-generator.desktop"

# Refresh icon cache / desktop database if the tools are present, so the
# new entry shows up immediately instead of after next login. Both are
# safe no-ops to skip if missing -- most distros will pick this up anyway.
command -v gtk-update-icon-cache >/dev/null 2>&1 && \
    gtk-update-icon-cache -q "$ICON_DIR" 2>/dev/null || true
command -v update-desktop-database >/dev/null 2>&1 && \
    update-desktop-database -q "$APPS_DIR" 2>/dev/null || true

echo "Installed. If '$BIN_DIR' isn't on your PATH, add:"
echo "  export PATH=\"\$HOME/.local/bin:\$PATH\""
echo "to your shell profile. You should also now find ComicInfo Generator"
echo "in your application launcher."
