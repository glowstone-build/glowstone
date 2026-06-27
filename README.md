# glowstone

An open-source **visualization software**. 

> Status: in development. Heavy work in progress.
> See [Roadmap](#roadmap).

## resources, documentation & references

You can find full documentation, research reference, installation instructions, requirements and other guides on [glowstone.build](https://glowstone.build)

## downloads

You can grab the latest release for your platform from the website above or [the Releases page.](https://github.com/glowstone-build/glowstone/releases/latest)

## capabilities

The software consists of a custom-built rendering engine called **Spectre**, which is optimised and designed for visualizing complex optical chains.

Showcase gallery is available at [glowstone.build/docs/resources/gallery](https://glowstone.build/docs/resources/gallery)

R&D References available at [glowstone.build/docs/resources/references](https://glowstone.build/docs/resources/references)

## motivation

I've built this project out of pure passion for lighting design and no access to cool-ass rigs or money to pay for expensive industry software to practise my programming skills; I also care about small details a lot, which is why I spent the majority amout of time engineering the rendering engine. You can read more about the capabilities of it at [glowstone.build/docs/rendering.](https://glowstone.build/docs/rendering).

All though I work as a full time SWE, I have zero background in graphical or rendering engineering, but had this exact project on my mind for a very long time. Thanks to AI capalibities I was able to ship a relatively well working PoC that I hope to develop further with the help of the OSS community!

If you have tokens to waste, feel free to glance over the `CONTRIBUTING.md` file. 

## roadmap

- Sequence/Video rendering + playback
- Documentation images (currently only placeholders, excluding gallery and title page)
- Object library (Truss framework, Objects, Crowds)
- DMX motors
- Improve fixture support (for more complex and non-standrard fixtures)
  - Framing shutters
- Plotting, Layers (CAD) + export to .pdf, auto-generate plots

## known issues

- Vulkan backend crashes.


## building & packaging

Run from source with `cargo run --release`. Release artifacts (icons, app bundles,
installers) are produced by the scripts in `scripts/`, writing to `dist/`:

| command | output |
| --- | --- |
| `scripts/gen-icons.sh <src.png>` | `assets/icons/glowstone.icns` + `glowstone.ico` from a large square source PNG. The macOS icon is a squircle on Apple's Big Sur grid. The icons are committed — only re-run this when the logo changes. |
| `scripts/package-macos.sh` | `Glowstone.app` + `Glowstone-<ver>-macos-<arch>.dmg` (ad-hoc signed). |
| `scripts/package-windows.sh` | `glowstone.exe` (icon embedded by `build.rs`) → portable `.zip`, plus an NSIS `setup.exe` when `makensis` is installed. |
| `scripts/package-all.sh` | every package this host can build. |
| `scripts/release.sh [tag]` | build the packages and publish a GitHub release (prerelease) with the artifacts. Defaults the tag to `v<ver>-alpha`. |

The Windows package **cross-compiles from macOS/Linux** via mingw-w64 — one-time setup:

```sh
brew install mingw-w64          # or apt-get install mingw-w64
rustup target add x86_64-pc-windows-gnu
```

The macOS `.dmg` is ad-hoc signed (runs locally); shipping to other Macs needs a
Developer ID signature + notarization (see the note printed by `package-macos.sh`).


## license

Glowstone is licensed under the GNU General Public License v3.0 (GPL-3.0).

See the `LICENSE` file for the full license text.

Contributions are accepted under the terms of the Contributor License Agreement (`CLA.md`).
