#!/usr/bin/env bash
# Build the release packages and publish a GitHub release (prerelease) with the
# artifacts attached.
#
#   scripts/release.sh [TAG]
#
# TAG defaults to v<Cargo version>-alpha. Builds the macOS .dmg (on macOS) and the
# cross-compiled Windows .zip / setup.exe, then creates the GitHub release on the
# repo's default branch. Re-running uploads (clobbers) the artifacts onto an existing
# release, so a Windows build that finishes later can be added with a second run.
#
# Env overrides: GLOWSTONE_REPO (default glowstone-build/glowstone),
# GLOWSTONE_RELEASE_TARGET (default main).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"(.*)".*/\1/')"
TAG="${1:-v${VERSION}-alpha}"
REPO="${GLOWSTONE_REPO:-glowstone-build/glowstone}"
TARGET="${GLOWSTONE_RELEASE_TARGET:-main}"

command -v gh >/dev/null || { echo "gh (GitHub CLI) not found — install it + run 'gh auth login'." >&2; exit 1; }

echo "==> building packages for $TAG"
if [ "$(uname)" = "Darwin" ]; then
  "$ROOT/scripts/package-macos.sh"
else
  echo "    (not macOS — skipping the .dmg)"
fi
# Windows cross-build is best-effort: a failure here must not abort the release.
"$ROOT/scripts/package-windows.sh" || echo "WARN: Windows package failed — releasing without it (re-run later to add it)."

shopt -s nullglob
ARTIFACTS=( dist/*.dmg dist/*.zip dist/*-setup.exe )
if [ ${#ARTIFACTS[@]} -eq 0 ]; then
  echo "no artifacts in dist/ — nothing to release" >&2
  exit 1
fi
echo "==> artifacts:"; printf '    %s\n' "${ARTIFACTS[@]}"

NOTES="## glowstone ${VERSION}

Downloads
- macOS (Apple Silicon): the .dmg. It's ad-hoc signed, so the first launch needs a
  right-click → Open (or \`xattr -dr com.apple.quarantine /Applications/Glowstone.app\`).
- Windows (x64): the portable `.zip`"

if gh release view "$TAG" --repo "$REPO" >/dev/null 2>&1; then
  echo "==> release $TAG exists — uploading artifacts (clobber)"
  gh release upload "$TAG" --repo "$REPO" --clobber "${ARTIFACTS[@]}"
else
  echo "==> creating prerelease $TAG on $REPO @ $TARGET"
  gh release create "$TAG" \
    --repo "$REPO" \
    --target "$TARGET" \
    --title "glowstone ${VERSION}-alpha" \
    --prerelease \
    --notes "$NOTES" \
    "${ARTIFACTS[@]}"
fi

echo "==> done:"
gh release view "$TAG" --repo "$REPO" --json url --jq .url 2>/dev/null || true
