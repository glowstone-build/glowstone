#!/usr/bin/env bash
# Regenerate the app icons (macOS .icns + Windows .ico) from a square source PNG.
#
#   scripts/gen-icons.sh [SOURCE_PNG]
#
# Default source: ~/Downloads/glowstone.png. Outputs to assets/icons/. The macOS
# `.icns` is packed with `iconutil` (macOS only); the Windows `.ico` is produced on
# any OS. A throwaway venv (scripts/.venv) holds Pillow + numpy.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="${1:-$HOME/Downloads/glowstone.png}"
OUT="$ROOT/assets/icons"
VENV="$ROOT/scripts/.venv"

[ -f "$SRC" ] || { echo "source image not found: $SRC" >&2; exit 1; }
mkdir -p "$OUT"

# --- Pillow/numpy in a local venv (created once) ----------------------------
if [ ! -x "$VENV/bin/python" ]; then
  echo "==> creating icon-build venv (Pillow + numpy)"
  python3 -m venv "$VENV"
  "$VENV/bin/pip" install --quiet --upgrade pip
  "$VENV/bin/pip" install --quiet Pillow numpy
fi

echo "==> generating icon images from $SRC"
"$VENV/bin/python" "$ROOT/scripts/gen_icons.py" "$SRC" "$OUT"

# --- pack the macOS .icns (needs iconutil; macOS only) ----------------------
if command -v iconutil >/dev/null 2>&1; then
  echo "==> packing glowstone.icns"
  iconutil -c icns "$OUT/AppIcon.iconset" -o "$OUT/glowstone.icns"
  echo "    $OUT/glowstone.icns"
else
  echo "==> iconutil not found (not macOS) — skipping .icns; .ico written"
fi

echo "==> done: $OUT/glowstone.icns  $OUT/glowstone.ico"
