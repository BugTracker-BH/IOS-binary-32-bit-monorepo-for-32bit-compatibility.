#!/bin/bash
# Assemble + sign + package the iOS build. Run after the app binary is linked.
#   ./package-ios.sh <linked-touchHLE-binary> [JellyCar.app]
set -euo pipefail
BIN="${1:?usage: package-ios.sh <touchHLE-binary> [game.app]}"
GAME="${2:-}"
HERE="$(cd "$(dirname "$0")" && pwd)"
THLE="$(cd "$HERE/.." && pwd)"
OUT="$HERE/_out"; APP="$OUT/Payload/touchHLE.app"
rm -rf "$OUT"; mkdir -p "$APP"
cp "$BIN" "$APP/touchHLE"; chmod +x "$APP/touchHLE"
cp "$HERE/Info.plist" "$APP/Info.plist"
cp -r "$THLE/touchHLE_dylibs" "$APP/touchHLE_dylibs"
cp -r "$THLE/touchHLE_fonts"  "$APP/touchHLE_fonts"
cp    "$THLE/touchHLE_default_options.txt" "$APP/touchHLE_default_options.txt"
if [ -n "$GAME" ] && [ -d "$GAME" ]; then
  mkdir -p "$APP/touchHLE_apps"; cp -r "$GAME" "$APP/touchHLE_apps/"
fi
ldid -S"$HERE/entitlements.plist" "$APP/touchHLE"
( cd "$OUT" && zip -qr9 "$HERE/touchHLE.ipa" Payload ); echo "Wrote $HERE/touchHLE.ipa"
# rootless .deb (Dopamine /var/jb layout)
DEB="$OUT/deb"; mkdir -p "$DEB/var/jb/Applications" "$DEB/DEBIAN"
cp -r "$APP" "$DEB/var/jb/Applications/touchHLE.app"
cat > "$DEB/DEBIAN/control" <<CTL
Package: org.touchhle.ios
Name: touchHLE
Version: 0.2.3
Architecture: iphoneos-arm64
Description: HLE emulator for 32-bit iPhone OS apps (JellyCar)
Maintainer: BugTracker-BH
Section: Games
CTL
dpkg-deb -Zgzip -b "$DEB" "$HERE/touchHLE_rootless.deb" >/dev/null && echo "Wrote $HERE/touchHLE_rootless.deb"
