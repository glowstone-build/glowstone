#!/usr/bin/env bash
# One-shot release: regenerate icons, then build every package this host can into
# dist/. macOS packaging (.app/.dmg) runs only on macOS; the Windows package
# cross-compiles wherever the mingw-w64 toolchain is available.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

"$ROOT/scripts/gen-icons.sh"

if [ "$(uname)" = "Darwin" ]; then
  "$ROOT/scripts/package-macos.sh"
else
  echo "==> skipping macOS package (not on macOS)"
fi

"$ROOT/scripts/package-windows.sh"

echo ""
echo "==> all packages:"
ls -lh "$ROOT/dist" 2>/dev/null || echo "  (none)"
