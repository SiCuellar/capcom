#!/usr/bin/env bash
# Build release and assemble + sign Capcom.app. Output: packaging/macos/build/Capcom.app
#
#   CAPCOM_UNIVERSAL=1      build arm64 + x86_64 and lipo them (needs both rustup targets)
#   CAPCOM_BIN=path         skip cargo and bundle this binary instead
#   CAPCOM_SIGN_IDENTITY=…  codesign identity (default "Capcom Local Signing"; ad-hoc if absent)
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
IDENTITY="${CAPCOM_SIGN_IDENTITY:-Capcom Local Signing}"
APP="$HERE/build/Capcom.app"

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
if [ -n "${CAPCOM_BIN:-}" ]; then
  BIN="$CAPCOM_BIN"
elif [ "${CAPCOM_UNIVERSAL:-0}" = "1" ]; then
  ( cd "$ROOT" && cargo build --release --target aarch64-apple-darwin && cargo build --release --target x86_64-apple-darwin )
  BIN="$ROOT/target/universal/capcom"
  mkdir -p "$(dirname "$BIN")"
  lipo -create -output "$BIN" "$ROOT/target/aarch64-apple-darwin/release/capcom" "$ROOT/target/x86_64-apple-darwin/release/capcom"
else
  ( cd "$ROOT" && cargo build --release )
  BIN="$ROOT/target/release/capcom"
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$HERE/Info.plist" "$APP/Contents/Info.plist"
cp "$BIN" "$APP/Contents/MacOS/capcom"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $VERSION" -c "Set :CFBundleShortVersionString $VERSION" "$APP/Contents/Info.plist"
[ -f "$HERE/icon/Capcom.icns" ] || "$HERE/icon/make-icon.sh"
cp "$HERE/icon/Capcom.icns" "$APP/Contents/Resources/Capcom.icns"

# Signing with a STABLE identity keeps the code requirement constant across rebuilds, so
# Accessibility / Input Monitoring / Microphone grants survive updates. Ad-hoc signatures change
# every build and macOS then silently drops the grants.
if security find-identity -v -p codesigning 2>/dev/null | grep -q "$IDENTITY"; then
  codesign --force --options runtime --entitlements "$HERE/Capcom.entitlements" --identifier dev.capcom.client --sign "$IDENTITY" "$APP"
  echo "signed with '$IDENTITY'"
else
  echo "note: signing identity '$IDENTITY' not found (create one with ./create-signing-cert.sh)." >&2
  echo "      Using ad-hoc signing: macOS may ask for permissions again after each rebuild/update." >&2
  codesign --force --options runtime --entitlements "$HERE/Capcom.entitlements" --identifier dev.capcom.client --sign - "$APP"
fi
codesign --verify --verbose=2 "$APP"
echo "built $APP"
