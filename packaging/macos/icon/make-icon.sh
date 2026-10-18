#!/usr/bin/env bash
# Regenerate the Capcom "Orbit" artwork from make-icon.swift (needs Xcode CLT: swift, iconutil):
#   Capcom.icns + preview.png (app icon), the menu-bar glyph PNGs embedded in the binary
#   (src/platform/macos/ui/icons/), and the showcase/exploration sheets in explorations/.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
swift "$HERE/make-icon.swift" iconset "$TMP/Capcom.iconset"
iconutil -c icns "$TMP/Capcom.iconset" -o "$HERE/Capcom.icns"
cp "$TMP/Capcom.iconset/icon_512x512.png" "$HERE/preview.png"
swift "$HERE/make-icon.swift" glyph "$ROOT/src/platform/macos/ui/icons"
mkdir -p "$HERE/explorations"
swift "$HERE/make-icon.swift" explore "$HERE/explorations/orbit-sheet.png"
swift "$HERE/make-icon.swift" final "$HERE/explorations/orbit-final.png"
echo "wrote $HERE/Capcom.icns"
