#!/usr/bin/env bash
# Install a development build of Spot for the current user (no root needed).
#
#   scripts/install-dev.sh            build (release) and install
#   scripts/install-dev.sh --no-build install the existing target/release/spot
#
# Same layout as the distro package, under ~/.local instead of /usr (see Makefile).
# Overridable (the test harness installs into an isolated session):
#   BINDIR (~/.local/bin), XDG_DATA_HOME (~/.local/share)
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BINDIR="${BINDIR:-$HOME/.local/bin}"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"

if [[ "${1:-}" != --no-build ]]; then
    make -C "$root" build
fi
make -s -C "$root" install PREFIX="$HOME/.local" BINDIR="$BINDIR" DATADIR="$DATA" MANDIR="$DATA/man"

cat <<MSG
Installed Spot into $BINDIR and $DATA.

Next:
  1. Log out and back in (GNOME on Wayland only discovers new extensions at login).
  2. Open "Spot" from the app grid. Its settings window turns on the helper extension,
     frees Super+Space if GNOME uses it for switching keyboard layouts, and can start
     Spot at login. Each of those is a button; nothing is changed until you press it.
MSG
