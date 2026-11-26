#!/usr/bin/env bash
# Build a Debian/Ubuntu package: target/deb/spot-launcher_<version>_<arch>.deb
#
#   scripts/build-deb.sh            build (release) and package
#   scripts/build-deb.sh --no-build package the existing target/release/spot
#
# Uses only dpkg's own tools (dpkg-deb, dpkg-shlibdeps), no cargo-deb. The file layout comes
# from `make install PREFIX=/usr`, the same one the per-user install uses.
# The Maintainer field comes from DEBFULLNAME / DEBEMAIL (the usual Debian variables); it is
# never guessed from your system.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

PKG=spot-launcher
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
ARCH=$(dpkg --print-architecture)
MAINTAINER="${DEBFULLNAME:-Spot developers} <${DEBEMAIL:-spot@localhost}>"
out=target/deb
stage=$out/root
deb=$out/${PKG}_${VERSION}_${ARCH}.deb

if [[ "${1:-}" != --no-build ]]; then
    make build
fi

rm -rf "$stage"
make -s install DESTDIR="$root/$stage" PREFIX=/usr

doc=$stage/usr/share/doc/$PKG
install -d "$doc"
gzip -9nc README.md > "$doc/README.md.gz"
cat > "$doc/copyright" <<EOF
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: spot
Source: (not yet published)

Files: *
Copyright: Spot developers
License: GPL-3.0-or-later
 This program is free software: you can redistribute it and/or modify
 it under the terms of the GNU General Public License as published by
 the Free Software Foundation, either version 3 of the License, or
 (at your option) any later version.
 .
 On Debian systems, the full text of the GNU General Public License
 version 3 can be found in the file '/usr/share/common-licenses/GPL-3'.
EOF
# Reproducible: changelog date from the metainfo release, not the clock.
release_date=$(sed -n 's/.*<release version="'"$VERSION"'" date="\([0-9-]*\)".*/\1/p' data/dev.spot.Spot.metainfo.xml)
cat <<EOF | gzip -9n > "$doc/changelog.gz"
$PKG ($VERSION) unstable; urgency=medium

  * Release $VERSION. See README.md.

 -- $MAINTAINER  $(date -R -u -d "${release_date:-1970-01-01} 12:00")
EOF
chmod 644 "$doc"/*

# Shared-library dependencies of the binary (libgtk-4-1, libadwaita-1-0, libc6, ...).
# dpkg-shlibdeps insists on a debian/control, so give it a throwaway one.
work=$out/shlibdeps
rm -rf "$work" && mkdir -p "$work/debian"
printf 'Source: %s\n\nPackage: %s\nArchitecture: any\n' "$PKG" "$PKG" > "$work/debian/control"
shlibs=$(cd "$work" && dpkg-shlibdeps -O "$root/$stage/usr/bin/spot" 2>/dev/null | sed -n 's/^shlibs:Depends=//p')
[[ -n "$shlibs" ]] || { echo "dpkg-shlibdeps found no dependencies" >&2; exit 1; }

install -d "$stage/DEBIAN"
cat > "$stage/DEBIAN/control" <<EOF
Package: $PKG
Version: $VERSION
Architecture: $ARCH
Maintainer: $MAINTAINER
Installed-Size: $(du -sk --exclude=DEBIAN "$stage" | cut -f1)
Depends: $shlibs
Recommends: gnome-shell (>= 48~)
Suggests: libfuse2t64 | libfuse2
Section: utils
Priority: optional
Description: Spotlight-style launcher for GNOME
 Press Super+Space, type a few letters and press Enter to start an installed
 application or an AppImage. Spot learns which results you pick, and also
 answers calculations, system actions (lock, log out, suspend, restart,
 power off), web addresses, file paths and web searches.
 .
 AppImages in ~/Applications, Downloads and Desktop are indexed without being
 run, and old ones that need libfuse2 are started in extract-and-run mode
 when libfuse2 is missing.
 .
 Includes the spot-helper GNOME Shell extension, which owns the shortcut. Each
 user turns it on once from Spot's settings window (open Spot from the app
 grid after logging out and back in).
EOF
# md5sums of everything shipped, relative to /.
(cd "$stage" && find usr -type f -print0 | LC_ALL=C sort -z | xargs -0 md5sum) > "$stage/DEBIAN/md5sums"
chmod 644 "$stage/DEBIAN/control" "$stage/DEBIAN/md5sums"

SOURCE_DATE_EPOCH=$(date -u -d "${release_date:-1970-01-01} 12:00" +%s) \
    dpkg-deb --root-owner-group -Zxz --build "$stage" "$deb" >/dev/null
echo "$deb"
