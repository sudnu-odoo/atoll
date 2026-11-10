#!/usr/bin/env bash
# Builds a .deb of Atoll (no sudo needed to build):
#
#   packaging/build-deb.sh      → target/debian/atoll_<version>_amd64.deb
#
# The package installs Atoll for every user on the machine:
#   /usr/bin/atoll
#   /usr/share/applications/atoll.desktop
#   /usr/share/icons/hicolor/scalable/apps/atoll.svg
#   /usr/share/gnome-shell/extensions/atoll-bridge@atoll.dev
# The bridge extension is installed but not enabled: each user turns it on in
# Atoll's Settings (GNOME extensions are per user).
set -euo pipefail

cd "$(dirname "$0")/.."
NAME=atoll
# CI passes the release version; local builds use Cargo.toml's.
VERSION=${ATOLL_VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)}
ARCH=$(dpkg --print-architecture)
EXT_UUID=atoll-bridge@atoll.dev
OUT=target/debian
ROOT="$OUT/${NAME}_${VERSION}_${ARCH}"

# Stamped into the binary, so Atoll knows which version it is (updates).
ATOLL_VERSION=$VERSION cargo build --release

rm -rf "$ROOT"
install -Dm755 target/release/atoll "$ROOT/usr/bin/atoll"
strip "$ROOT/usr/bin/atoll"
install -Dm644 resources/atoll.svg "$ROOT/usr/share/icons/hicolor/scalable/apps/atoll.svg"
install -d "$ROOT/usr/share/applications"
sed "s|@BIN@|/usr/bin/atoll|" resources/atoll.desktop > "$ROOT/usr/share/applications/atoll.desktop"
install -d "$ROOT/usr/share/gnome-shell/extensions/$EXT_UUID"
install -m644 "gnome-extension/$EXT_UUID/"* "$ROOT/usr/share/gnome-shell/extensions/$EXT_UUID/"
install -Dm644 README.md "$ROOT/usr/share/doc/$NAME/README.md"

# Library dependencies, worked out from the binary itself.
mkdir -p "$OUT/shlibs/debian"
touch "$OUT/shlibs/debian/control"
LIBS=$(cd "$OUT/shlibs" && dpkg-shlibdeps -O "$OLDPWD/$ROOT/usr/bin/atoll" 2>/dev/null \
    | sed -n 's/^shlibs:Depends=//p')

install -d "$ROOT/DEBIAN"
cat > "$ROOT/DEBIAN/control" <<CONTROL
Package: $NAME
Version: $VERSION
Architecture: $ARCH
Maintainer: Atoll <sudnu@odoo.com>
Depends: ${LIBS:-libgtk-4-1 (>= 4.14)}, gnome-shell (>= 46)
Recommends: wireplumber
Section: utils
Priority: optional
Installed-Size: $(du -sk "$ROOT/usr" | cut -f1)
Description: Dynamic Island for the GNOME top bar
 A notch over the GNOME top bar (Xorg) with music, synced lyrics,
 notifications, quick controls, calendar, to-dos, water and a file shelf.
 Runs as its own process; includes the optional Atoll Bridge extension,
 enabled per user from Atoll's Settings.
CONTROL

dpkg-deb --root-owner-group --build "$ROOT" >/dev/null
echo "Built $ROOT.deb"
