//! Build script: embeds the Windows app icon (+ version metadata) into the `.exe`.
//!
//! It runs on the HOST but gates on the TARGET, so it is a no-op for native
//! macOS / Linux builds and only does work when compiling for Windows (incl. the
//! `x86_64-pc-windows-gnu` cross build from macOS via mingw-w64).
use std::env;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let ico = "assets/icons/glowstone.ico";
    println!("cargo:rerun-if-changed={ico}");
    if !std::path::Path::new(ico).exists() {
        println!(
            "cargo:warning=missing {ico} — run scripts/gen-icons.sh (the .exe will have no icon)"
        );
        return;
    }

    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico);
    // Cross-compiling with mingw-w64 (e.g. from macOS): the only resource/archiver
    // tools on PATH are the target-prefixed ones, so point winresource at them.
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu") {
        let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_else(|_| "x86_64".into());
        res.set_windres_path(&format!("{arch}-w64-mingw32-windres"));
        res.set_ar_path(&format!("{arch}-w64-mingw32-ar"));
    }
    if let Err(e) = res.compile() {
        println!("cargo:warning=failed to embed Windows resources: {e}");
    }
}
