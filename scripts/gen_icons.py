#!/usr/bin/env python3
"""Generate the glowstone app icons (macOS + Windows) from one square source PNG.

macOS (`AppIcon.iconset/` → `iconutil` packs the `.icns` in the wrapper script):
  Apple's HIG wants macOS icons to BAKE IN the rounded shape — the system does not
  mask them (unlike iOS). We follow the Big Sur grid: a 1024px canvas with the icon
  art on an 824px rounded square centred inside it (100px transparent margin all
  round) so it sits at the same visual size as the system apps in the Dock. The
  corner is a superellipse ("squircle", n≈5) — the continuous-curvature look Apple
  uses, not a plain circular-corner radius.

Windows (`.ico`): square, full-bleed, multi-resolution. Windows tiles are square
  (the shell rounds them itself on 11), so no squircle here.

Usage: gen_icons.py <source.png> <out_dir>
"""
import sys
from pathlib import Path

import numpy as np
from PIL import Image, ImageChops

# macOS Big Sur icon grid (px, on a 1024 canvas).
CANVAS = 1024
CONTENT = 824                     # the rounded square
MARGIN = (CANVAS - CONTENT) // 2  # 100px transparent border
SQUIRCLE_N = 5.0                  # superellipse exponent ≈ Apple's continuous corner

# (filename, pixel size) for an Apple `.iconset` — iconutil packs these into .icns.
ICONSET = [
    ("icon_16x16.png", 16), ("icon_16x16@2x.png", 32),
    ("icon_32x32.png", 32), ("icon_32x32@2x.png", 64),
    ("icon_128x128.png", 128), ("icon_128x128@2x.png", 256),
    ("icon_256x256.png", 256), ("icon_256x256@2x.png", 512),
    ("icon_512x512.png", 512), ("icon_512x512@2x.png", 1024),
]
WIN_SIZES = [256, 128, 64, 48, 32, 16]


def squircle_mask(size: int, n: float = SQUIRCLE_N, ss: int = 4) -> Image.Image:
    """An anti-aliased superellipse alpha mask filling a `size`×`size` square."""
    s = size * ss
    axis = (np.arange(s) + 0.5) / s * 2.0 - 1.0      # pixel centres in -1..1
    x, y = np.meshgrid(axis, axis)
    inside = (np.abs(x) ** n + np.abs(y) ** n) <= 1.0
    hi = Image.fromarray((inside * 255).astype("uint8"), "L")
    return hi.resize((size, size), Image.LANCZOS)    # downsample → smooth edge


def main() -> None:
    src_path, out_dir = Path(sys.argv[1]).expanduser(), Path(sys.argv[2])
    src = Image.open(src_path).convert("RGBA")
    if src.width != src.height:
        side = min(src.width, src.height)             # centre-crop to square
        left, top = (src.width - side) // 2, (src.height - side) // 2
        src = src.crop((left, top, left + side, top + side))

    iconset = out_dir / "AppIcon.iconset"
    iconset.mkdir(parents=True, exist_ok=True)

    # --- macOS: squircle art inset on the 1024 grid -------------------------
    content = src.resize((CONTENT, CONTENT), Image.LANCZOS)
    masked_alpha = ImageChops.multiply(content.getchannel("A"), squircle_mask(CONTENT))
    content.putalpha(masked_alpha)
    master = Image.new("RGBA", (CANVAS, CANVAS), (0, 0, 0, 0))
    master.paste(content, (MARGIN, MARGIN), content)
    master.save(out_dir / "glowstone-macos-1024.png")

    for name, size in ICONSET:
        master.resize((size, size), Image.LANCZOS).save(iconset / name)

    # --- Windows: square, full-bleed, multi-resolution .ico -----------------
    base = src.resize((256, 256), Image.LANCZOS)
    base.save(out_dir / "glowstone.ico", sizes=[(s, s) for s in WIN_SIZES])

    print(f"wrote {iconset}/ (+{len(ICONSET)} pngs), glowstone.ico, glowstone-macos-1024.png")


if __name__ == "__main__":
    main()
