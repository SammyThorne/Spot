#!/usr/bin/env bash
# Remove what install-dev.sh installed. Same overrides: BINDIR, XDG_DATA_HOME, XDG_CONFIG_HOME.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BINDIR="${BINDIR:-$HOME/.local/bin}"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
CONFIG="${XDG_CONFIG_HOME:-$HOME/.config}"

gnome-extensions disable spot-helper@spot.dev 2>/dev/null || true
"$BINDIR/spot" --quit 2>/dev/null || true
make -s -C "$root" uninstall PREFIX="$HOME/.local" BINDIR="$BINDIR" DATADIR="$DATA" MANDIR="$DATA/man"
rm -fv "$CONFIG/autostart/dev.spot.Spot.desktop"
cat <<MSG
Removed. Your history, settings and AppImage cache are kept:
  $DATA/spot  $CONFIG/spot  ${XDG_CACHE_HOME:-$HOME/.cache}/spot
If Spot's settings freed Super+Space from the layout switcher, restore it with:
  gsettings reset org.gnome.desktop.wm.keybindings switch-input-source
MSG
