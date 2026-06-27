#!/usr/bin/env bash
# Build the Windows release and package it: a portable .zip always, plus an NSIS
# installer .exe when `makensis` is available.
#
#   scripts/package-windows.sh
#
# From macOS/Linux this CROSS-compiles the `x86_64-pc-windows-gnu` target (needs
# mingw-w64 + `rustup target add x86_64-pc-windows-gnu`; see .cargo/config.toml). On a
# native Windows host, point TARGET at the MSVC target instead (set TARGET=…-msvc).
# The app icon is embedded into the .exe by build.rs.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

APP_NAME="Glowstone"
BIN="glowstone.exe"
TARGET="${TARGET:-x86_64-pc-windows-gnu}"
VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"(.*)".*/\1/')"
DIST="$ROOT/dist"
ICO="assets/icons/glowstone.ico"

[ -f "$ICO" ] || { echo "missing $ICO — run scripts/gen-icons.sh first" >&2; exit 1; }

if ! rustup target list --installed | grep -qx "$TARGET"; then
  echo "==> adding rust target $TARGET"
  rustup target add "$TARGET"
fi

echo "==> building release binary ($TARGET)"
cargo build --release --target "$TARGET"

EXE="target/$TARGET/release/$BIN"
[ -f "$EXE" ] || { echo "build produced no $EXE" >&2; exit 1; }

mkdir -p "$DIST"

# --- portable .zip (always) -------------------------------------------------
echo "==> packaging portable zip"
STAGE="$(mktemp -d)/$APP_NAME"
mkdir -p "$STAGE"
cp "$EXE" "$STAGE/$BIN"
cp README.md LICENSE "$STAGE/" 2>/dev/null || true
ZIP="$DIST/$APP_NAME-$VERSION-windows-x64.zip"
rm -f "$ZIP"
( cd "$(dirname "$STAGE")" && zip -rq "$ZIP" "$APP_NAME" )
echo "    $ZIP"

# --- NSIS installer (when makensis is installed) ----------------------------
if command -v makensis >/dev/null 2>&1; then
  echo "==> building NSIS installer"
  SETUP="$DIST/$APP_NAME-$VERSION-windows-x64-setup.exe"
  makensis -V2 \
    "-DVERSION=$VERSION" \
    "-DAPPNAME=$APP_NAME" \
    "-DEXE=$ROOT/$EXE" \
    "-DICON=$ROOT/$ICO" \
    "-DOUTFILE=$SETUP" \
    "$ROOT/scripts/glowstone.nsi"
  echo "    $SETUP"
else
  echo "==> makensis not found — skipping installer (portable zip above)."
  echo "    Install NSIS to build a setup.exe:  brew install makensis   (or apt/choco)"
fi

echo ""
echo "==> done (dist/)"
