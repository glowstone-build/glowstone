//! NDI (Network Device Interface) receive support for LED walls.
//!
//! NDI's SDK is proprietary and is NOT bundled in this repo. Receive is enabled
//! with the optional `ndi` Cargo feature, which links the user-installed NDI 6
//! runtime via the `grafton-ndi` crate (building that feature needs the NDI SDK
//! + Clang present). The DEFAULT build compiles WITHOUT the SDK and uses the
//! graceful no-op [`NdiClient`] stub below, so the app always runs and the NDI
//! source picker simply reports "unavailable". See `docs/RESEARCH-led-ndi.md`.
//!
//! Both builds expose the same [`NdiClient`] API as the CITP client:
//! `server_names()` for discovery, `frame_for(source)` to pull the latest frame,
//! and `retain(active)` to stop streams no longer referenced.

use std::sync::Arc;

use crate::scene::screen::ScreenFrame;

#[cfg(not(feature = "ndi"))]
mod imp {
    use super::*;

    /// No-op NDI client (the `ndi` feature is not compiled in). NDI sources are
    /// never discovered and no frames are produced.
    pub struct NdiClient;

    impl NdiClient {
        pub fn new() -> Self {
            NdiClient
        }
        pub fn available(&self) -> bool {
            false
        }
        pub fn server_names(&self) -> Vec<String> {
            Vec::new()
        }
        pub fn frame_for(&mut self, _source: &str) -> Option<Arc<ScreenFrame>> {
            None
        }
        pub fn retain(&mut self, _active: &[String]) {}
    }
}

#[cfg(feature = "ndi")]
mod imp {
    //! Real NDI receive via grafton-ndi. NOTE: this path is only compiled with
    //! `--features ndi` (which requires the NDI SDK), so it is NOT exercised by
    //! the default CI build. The grafton-ndi API surface here follows the crate's
    //! 1.x docs; confirm the exact `VideoFrame` accessor names (`data`,
    //! `line_stride`) against `cargo doc -p grafton-ndi` when building with the SDK.
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

    use grafton_ndi::{
        Finder, FinderOptions, Receiver, ReceiverColorFormat, ReceiverOptions, NDI,
    };

    struct Stream {
        frame: Arc<Mutex<Option<Arc<ScreenFrame>>>>,
        stop: Arc<AtomicBool>,
    }

    pub struct NdiClient {
        ndi: Option<Arc<NDI>>,
        sources: Arc<Mutex<Vec<String>>>,
        streams: HashMap<String, Stream>,
        finder_stop: Arc<AtomicBool>,
    }

    impl NdiClient {
        pub fn new() -> Self {
            match NDI::new() {
                Ok(ndi) => {
                    let ndi = Arc::new(ndi);
                    let sources = Arc::new(Mutex::new(Vec::new()));
                    let finder_stop = Arc::new(AtomicBool::new(false));
                    spawn_finder(ndi.clone(), sources.clone(), finder_stop.clone());
                    NdiClient { ndi: Some(ndi), sources, streams: HashMap::new(), finder_stop }
                }
                Err(e) => {
                    log::warn!("NDI runtime not available: {e}");
                    NdiClient {
                        ndi: None,
                        sources: Arc::new(Mutex::new(Vec::new())),
                        streams: HashMap::new(),
                        finder_stop: Arc::new(AtomicBool::new(false)),
                    }
                }
            }
        }
        pub fn available(&self) -> bool {
            self.ndi.is_some()
        }
        pub fn server_names(&self) -> Vec<String> {
            self.sources.lock().unwrap().clone()
        }
        pub fn frame_for(&mut self, source: &str) -> Option<Arc<ScreenFrame>> {
            let ndi = self.ndi.clone()?;
            if source.is_empty() {
                return None;
            }
            if !self.streams.contains_key(source) {
                self.streams.insert(source.to_string(), spawn_receiver(ndi, source.to_string()));
            }
            self.streams.get(source).and_then(|s| s.frame.lock().unwrap().clone())
        }
        pub fn retain(&mut self, active: &[String]) {
            self.streams.retain(|k, s| {
                let keep = active.iter().any(|a| a == k);
                if !keep {
                    s.stop.store(true, Ordering::Relaxed);
                }
                keep
            });
        }
    }

    impl Drop for NdiClient {
        fn drop(&mut self) {
            self.finder_stop.store(true, Ordering::Relaxed);
            for s in self.streams.values() {
                s.stop.store(true, Ordering::Relaxed);
            }
        }
    }

    fn finder_opts() -> FinderOptions {
        FinderOptions::builder().show_local_sources(true).build()
    }

    fn spawn_finder(ndi: Arc<NDI>, sources: Arc<Mutex<Vec<String>>>, stop: Arc<AtomicBool>) {
        std::thread::Builder::new()
            .name("ndi-finder".into())
            .spawn(move || {
                let finder = match Finder::new(&ndi, &finder_opts()) {
                    Ok(f) => f,
                    Err(e) => {
                        log::warn!("NDI finder: {e}");
                        return;
                    }
                };
                while !stop.load(Ordering::Relaxed) {
                    if let Ok(list) = finder.current_sources() {
                        let names: Vec<String> = list.iter().map(|s| format!("{s}")).collect();
                        *sources.lock().unwrap() = names;
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
            })
            .ok();
    }

    fn spawn_receiver(ndi: Arc<NDI>, source_name: String) -> Stream {
        let frame = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let (frame_t, stop_t) = (frame.clone(), stop.clone());
        // The discovered name is "NAME@url"; the url can vary, so match on the
        // stable NAME part (before the last '@').
        let want = source_name.rsplit_once('@').map(|(n, _)| n.to_string()).unwrap_or(source_name);
        std::thread::Builder::new()
            .name("ndi-recv".into())
            .spawn(move || {
                let mut generation = 0u64;
                // ONE persistent finder so discovery accumulates (a fresh finder per
                // retry + the no-wait `current_sources()` snapshot never finds it).
                let finder = match Finder::new(&ndi, &finder_opts()) {
                    Ok(f) => f,
                    Err(e) => {
                        log::warn!("NDI finder (recv): {e}");
                        return;
                    }
                };
                while !stop_t.load(Ordering::Relaxed) {
                    // Give discovery time to populate, then snapshot + match by name.
                    let _ = finder.wait_for_sources(Duration::from_secs(2));
                    let source = finder.current_sources().ok().and_then(|list| {
                        list.into_iter().find(|s| {
                            let disp = format!("{s}");
                            let nm = disp.rsplit_once('@').map(|(n, _)| n).unwrap_or(disp.as_str());
                            nm == want.as_str()
                        })
                    });
                    let Some(source) = source else {
                        std::thread::sleep(Duration::from_millis(300));
                        continue;
                    };
                    // Request RGBA so the bytes upload straight to an Rgba8 texture.
                    let opts =
                        ReceiverOptions::builder(source).color(ReceiverColorFormat::RGBX_RGBA).build();
                    let receiver = match Receiver::new(&ndi, &opts) {
                        Ok(r) => r,
                        Err(e) => {
                            log::debug!("NDI receiver: {e}");
                            std::thread::sleep(Duration::from_millis(500));
                            continue;
                        }
                    };
                    let mut misses = 0u32;
                    while !stop_t.load(Ordering::Relaxed) {
                        match receiver.video().capture(Duration::from_millis(1000)) {
                            Ok(v) => {
                                misses = 0;
                                // grafton-ndi: width()/height() are i32; stride comes
                                // from line_stride_or_size() (an enum, since compressed
                                // formats report a total size instead of a row stride).
                                let w = v.width().max(0) as u32;
                                let h = v.height().max(0) as u32;
                                let stride = match v.line_stride_or_size() {
                                    grafton_ndi::LineStrideOrSize::LineStrideBytes(s) => {
                                        s.max(0) as usize
                                    }
                                    grafton_ndi::LineStrideOrSize::DataSizeBytes(_) => {
                                        (w as usize) * 4
                                    }
                                };
                                let rgba = pack_rgba(v.data(), w, h, stride);
                                if !rgba.is_empty() {
                                    generation = generation.wrapping_add(1);
                                    *frame_t.lock().unwrap() = Some(Arc::new(ScreenFrame {
                                        width: w,
                                        height: h,
                                        rgba,
                                        generation,
                                    }));
                                }
                            }
                            Err(_) => {
                                // Timeout / connecting. After several misses assume the
                                // source went away and re-resolve it.
                                misses += 1;
                                if misses > 5 {
                                    break;
                                }
                            }
                        }
                    }
                }
            })
            .ok();
        Stream { frame, stop }
    }

    /// Copy an RGBA frame (possibly row-padded) into a tightly-packed buffer.
    fn pack_rgba(data: &[u8], w: u32, h: u32, stride: usize) -> Vec<u8> {
        let row = (w as usize) * 4;
        let stride = if stride == 0 { row } else { stride };
        // Reject a sub-row stride too (the per-row copy below reads `row` bytes
        // from each `stride`-spaced offset).
        if w == 0 || h == 0 || stride < row || data.len() < stride * (h as usize) {
            return Vec::new();
        }
        if stride == row {
            return data[..row * h as usize].to_vec();
        }
        let mut out = vec![0u8; row * h as usize];
        for y in 0..h as usize {
            out[y * row..(y + 1) * row].copy_from_slice(&data[y * stride..y * stride + row]);
        }
        out
    }
}

pub use imp::NdiClient;
