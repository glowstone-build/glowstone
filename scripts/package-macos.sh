#!/usr/bin/env bash
# Build a macOS release and package it as Glowstone.app inside a .dmg.
#
#   scripts/package-macos.sh
#
# Output: dist/Glowstone-<version>-macos-<arch>.dmg  (+ the staged Glowstone.app).
# The app is ad-hoc code-signed so it runs locally; shipping it to OTHER Macs needs a
# Developer ID signature + notarization (see the note at the end).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

APP_NAME="Glowstone"
BIN="glowstone"
BUNDLE_ID="build.glowstone.glowstone"
VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"(.*)".*/\1/')"
ARCH="$(uname -m)"                       # arm64 / x86_64
ICNS="assets/icons/glowstone.icns"
DIST="$ROOT/dist"
APP="$DIST/$APP_NAME.app"

[ -f "$ICNS" ] || { echo "missing $ICNS — run scripts/gen-icons.sh first" >&2; exit 1; }

echo "==> building release binary ($ARCH)"
cargo build --release

echo "==> assembling $APP_NAME.app ($VERSION)"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "target/release/$BIN" "$APP/Contents/MacOS/$BIN"
cp "$ICNS" "$APP/Contents/Resources/glowstone.icns"
printf 'APPL????' > "$APP/Contents/PkgInfo"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>            <string>$APP_NAME</string>
  <key>CFBundleDisplayName</key>     <string>$APP_NAME</string>
  <key>CFBundleIdentifier</key>      <string>$BUNDLE_ID</string>
  <key>CFBundleExecutable</key>      <string>$BIN</string>
  <key>CFBundleIconFile</key>        <string>glowstone</string>
  <key>CFBundleShortVersionString</key> <string>$VERSION</string>
  <key>CFBundleVersion</key>         <string>$VERSION</string>
  <key>CFBundlePackageType</key>     <string>APPL</string>
  <key>CFBundleInfoDictionaryVersion</key> <string>6.0</string>
  <key>LSMinimumSystemVersion</key>  <string>11.0</string>
  <key>NSHighResolutionCapable</key> <true/>
  <key>LSApplicationCategoryType</key> <string>public.app-category.graphics-design</string>
  <key>NSHumanReadableCopyright</key> <string>glowstone.build — MIT OR Apache-2.0</string>
</dict>
</plist>
PLIST

echo "==> ad-hoc code-signing"
codesign --force --deep --sign - "$APP"

echo "==> creating .dmg"
DMG="$DIST/$APP_NAME-$VERSION-macos-$ARCH.dmg"
STAGE="$(mktemp -d)"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"          # drag-to-install layout
rm -f "$DMG"
hdiutil create -volname "$APP_NAME" -srcfolder "$STAGE" -ov -format UDZO "$DMG" >/dev/null
rm -rf "$STAGE"

echo ""
echo "==> done:"
echo "    $APP"
echo "    $DMG"
echo ""
echo "    NOTE: this build is ad-hoc signed (runs on THIS Mac). To distribute to other"
echo "    Macs without Gatekeeper warnings, sign with a Developer ID and notarize:"
echo "      codesign --force --deep --options runtime --sign \"Developer ID Application: …\" \"$APP\""
echo "      xcrun notarytool submit \"$DMG\" --keychain-profile … --wait && xcrun stapler staple \"$DMG\""
