#!/bin/sh
# Install a desktop entry and icons for Clutch into the user's XDG directories, so Linux
# desktops show its icon. On Wayland, KDE (and GNOME) take the icon from the .desktop file whose
# name matches the window's app ID ("clutch"); the icon the window sets itself is ignored.
# Release packages (`cargo tauri build` with bundling on) install their own entry instead.
#
# Usage: scripts/install-desktop-entry.sh [path/to/clutch]   (default: target/debug/clutch)
#        scripts/install-desktop-entry.sh --uninstall
set -eu
cd "$(dirname "$0")/.."

data="${XDG_DATA_HOME:-$HOME/.local/share}"
icons="$data/icons/hicolor"
entry="$data/applications/clutch.desktop"

refresh() {
    # Make KDE re-read desktop entries and icons now rather than at next login.
    if command -v kbuildsycoca6 >/dev/null; then kbuildsycoca6 >/dev/null 2>&1 || true; fi
    if command -v update-desktop-database >/dev/null; then update-desktop-database "$data/applications" 2>/dev/null || true; fi
}

if [ "${1:-}" = "--uninstall" ]; then
    rm -f "$entry" "$icons"/*/apps/clutch.png "$icons/scalable/apps/clutch.svg"
    refresh
    echo "Removed $entry and Clutch's icons."
    exit 0
fi

binary="$(realpath "${1:-target/debug/clutch}")"
[ -x "$binary" ] || { echo "No executable at $binary; build it first (cargo build -p clutch)." >&2; exit 1; }

for size in 32 64 128; do
    install -Dm644 "src-tauri/icons/${size}x${size}.png" "$icons/${size}x${size}/apps/clutch.png"
done
install -Dm644 "src-tauri/icons/128x128@2x.png" "$icons/256x256/apps/clutch.png"
install -Dm644 "src-tauri/icons/icon.png" "$icons/512x512/apps/clutch.png"
install -Dm644 "ui/icons/clutch-logo.svg" "$icons/scalable/apps/clutch.svg"

mkdir -p "$(dirname "$entry")"
cat > "$entry" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Clutch
Comment=Torrent client that routes through a WireGuard VPN
Exec="$binary"
Icon=clutch
Terminal=false
Categories=Network;FileTransfer;P2P;
StartupWMClass=clutch
DESKTOP

refresh
echo "Installed $entry (runs $binary) and icons under $icons."
