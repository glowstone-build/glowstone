//! NDI (Network Device Interface) receive support for LED walls.
//!
//! The NDI runtime is proprietary and is NOT bundled. We integrate it the way the
//! SDK intends for redistributable apps: the `ndi` feature **loads the runtime at
//! runtime** (`dlopen`/`LoadLibrary` via [`libloading`]) and looks up the exported
//! `NDIlib_*` functions. Nothing is linked at build time, so:
//!
//! * the binary has NO hard dependency on `libndi` — it always launches, even with
//!   the runtime absent, and simply reports NDI "unavailable" (graceful no-op);
//! * building the `ndi` feature needs NO NDI SDK (only `libloading`), so it ships in
//!   the default macOS + Windows builds. The user installs the NDI runtime to enable
//!   NDI sources.
//!
//! A `--no-default-features` build drops NDI entirely (the stub below). Both paths
//! expose the same [`NdiClient`] API as the CITP client: `server_names()` for
//! discovery, `frame_for(source)` to pull the latest frame, and `retain(active)` to
//! stop streams no longer referenced. See `docs/RESEARCH-led-ndi.md`.

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
    //! Real NDI receive — the runtime is `dlopen`ed at startup (see the module docs),
    //! so an absent runtime is a graceful no-op and there is no build-time SDK link.
    use super::*;
    use std::collections::HashMap;
    use std::ffi::{c_void, CStr, CString};
    use std::os::raw::c_char;
    use std::ptr;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;
    use std::thread;
    use std::time::Duration;

    /// Raw FFI mirror of the NDI 6 C API (receive subset). Layout matches
    /// `Processing.NDI.structs.h`; we only declare what we call.
    mod sys {
        use std::os::raw::c_char;

        #[repr(C)]
        pub struct Source {
            pub p_ndi_name: *const c_char,
            // C union of `p_url_address` / `p_ip_address` — both `const char*`.
            pub p_url_address: *const c_char,
        }

        #[repr(C)]
        pub struct FindCreate {
            pub show_local_sources: bool,
            pub p_groups: *const c_char,
            pub p_extra_ips: *const c_char,
        }

        #[repr(C)]
        pub struct RecvCreateV3 {
            pub source_to_connect_to: Source,
            pub color_format: i32, // NDIlib_recv_color_format_e
            pub bandwidth: i32,    // NDIlib_recv_bandwidth_e
            pub allow_video_fields: bool,
            pub p_ndi_recv_name: *const c_char,
        }

        #[repr(C)]
        pub struct VideoFrame {
            pub xres: i32,
            pub yres: i32,
            pub four_cc: i32, // NDIlib_FourCC_video_type_e
            pub frame_rate_n: i32,
            pub frame_rate_d: i32,
            pub picture_aspect_ratio: f32,
            pub frame_format_type: i32, // NDIlib_frame_format_type_e
            pub timecode: i64,
            pub p_data: *mut u8,
            // C union of `line_stride_in_bytes` / `data_size_in_bytes` — both `int`.
            pub line_stride_or_size: i32,
            pub p_metadata: *const c_char,
            pub timestamp: i64,
        }

        // Enum values we use (NDIlib_recv_color_format_e / _bandwidth_e / frame_type_e).
        pub const COLOR_RGBX_RGBA: i32 = 2;
        pub const BANDWIDTH_HIGHEST: i32 = 100;
        pub const FRAME_TYPE_NONE: i32 = 0;
        pub const FRAME_TYPE_VIDEO: i32 = 1;
        pub const FRAME_TYPE_ERROR: i32 = 4;
    }

    type Initialize = unsafe extern "C" fn() -> bool;
    type FindCreateV2 = unsafe extern "C" fn(*const sys::FindCreate) -> *mut c_void;
    type FindGetCurrentSources = unsafe extern "C" fn(*mut c_void, *mut u32) -> *const sys::Source;
    type FindWaitForSources = unsafe extern "C" fn(*mut c_void, u32) -> bool;
    type FindDestroy = unsafe extern "C" fn(*mut c_void);
    type RecvCreateV3 = unsafe extern "C" fn(*const sys::RecvCreateV3) -> *mut c_void;
    type RecvCaptureV3 =
        unsafe extern "C" fn(*mut c_void, *mut sys::VideoFrame, *mut c_void, *mut c_void, u32) -> i32;
    type RecvFreeVideoV2 = unsafe extern "C" fn(*mut c_void, *const sys::VideoFrame);
    type RecvDestroy = unsafe extern "C" fn(*mut c_void);

    /// The `dlopen`ed runtime + its resolved entry points. Held behind an `Arc` and
    /// shared by the finder / receiver threads; the `_lib` field keeps the library
    /// loaded so the function pointers stay valid. (`Library` + `fn` pointers are
    /// `Send`+`Sync`; the opaque instance pointers never cross threads.)
    struct NdiLib {
        _lib: libloading::Library,
        find_create_v2: FindCreateV2,
        find_get_current_sources: FindGetCurrentSources,
        find_wait_for_sources: FindWaitForSources,
        find_destroy: FindDestroy,
        recv_create_v3: RecvCreateV3,
        recv_capture_v3: RecvCaptureV3,
        recv_free_video_v2: RecvFreeVideoV2,
        recv_destroy: RecvDestroy,
    }

    impl NdiLib {
        fn load() -> Option<NdiLib> {
            let lib = open_runtime()?;
            // SAFETY: the names are real exports of the NDI runtime; any miss → None.
            unsafe {
                macro_rules! sym {
                    ($t:ty, $n:expr) => {
                        *lib.get::<$t>($n).ok()?
                    };
                }
                let initialize: Initialize = sym!(Initialize, b"NDIlib_initialize\0");
                let nlib = NdiLib {
                    find_create_v2: sym!(FindCreateV2, b"NDIlib_find_create_v2\0"),
                    find_get_current_sources: sym!(
                        FindGetCurrentSources,
                        b"NDIlib_find_get_current_sources\0"
                    ),
                    find_wait_for_sources: sym!(FindWaitForSources, b"NDIlib_find_wait_for_sources\0"),
                    find_destroy: sym!(FindDestroy, b"NDIlib_find_destroy\0"),
                    recv_create_v3: sym!(RecvCreateV3, b"NDIlib_recv_create_v3\0"),
                    recv_capture_v3: sym!(RecvCaptureV3, b"NDIlib_recv_capture_v3\0"),
                    recv_free_video_v2: sym!(RecvFreeVideoV2, b"NDIlib_recv_free_video_v2\0"),
                    recv_destroy: sym!(RecvDestroy, b"NDIlib_recv_destroy\0"),
                    _lib: lib,
                };
                // `initialize` returns false on an unsupported CPU → treat as absent.
                if !initialize() {
                    return None;
                }
                Some(nlib)
            }
        }
    }

    /// Try the well-known runtime library names + the SDK's `NDI_RUNTIME_DIR_V*`
    /// locations. Returns the first that loads, or `None` (→ graceful no-op).
    fn open_runtime() -> Option<libloading::Library> {
        let file = if cfg!(target_os = "windows") {
            "Processing.NDI.Lib.x64.dll"
        } else if cfg!(target_os = "macos") {
            "libndi.dylib"
        } else {
            "libndi.so.6"
        };
        let mut candidates: Vec<String> = vec![file.to_string()];
        for var in ["NDI_RUNTIME_DIR_V6", "NDI_RUNTIME_DIR_V5", "NDI_RUNTIME_DIR_V4"] {
            if let Ok(dir) = std::env::var(var) {
                let sep = if cfg!(target_os = "windows") { '\\' } else { '/' };
                candidates.push(format!("{dir}{sep}{file}"));
            }
        }
        if cfg!(target_os = "macos") {
            candidates.push("/usr/local/lib/libndi.dylib".into());
            candidates.push("/Library/NDI SDK for Apple/lib/macOS/libndi.dylib".into());
        } else if cfg!(target_os = "linux") {
            candidates.push("libndi.so.5".into());
            candidates.push("libndi.so".into());
        }
        for name in candidates {
            // SAFETY: loading a shared library; if it isn't a valid NDI runtime the
            // later symbol lookups fail and we move on.
            if let Ok(lib) = unsafe { libloading::Library::new(&name) } {
                return Some(lib);
            }
        }
        None
    }

    struct Stream {
        frame: Arc<Mutex<Option<Arc<ScreenFrame>>>>,
        stop: Arc<AtomicBool>,
    }

    pub struct NdiClient {
        lib: Option<Arc<NdiLib>>,
        sources: Arc<Mutex<Vec<String>>>,
        streams: HashMap<String, Stream>,
        finder_stop: Arc<AtomicBool>,
    }

    impl NdiClient {
        pub fn new() -> Self {
            match NdiLib::load() {
                Some(lib) => {
                    let lib = Arc::new(lib);
                    let sources = Arc::new(Mutex::new(Vec::new()));
                    let finder_stop = Arc::new(AtomicBool::new(false));
                    spawn_finder(lib.clone(), sources.clone(), finder_stop.clone());
                    NdiClient { lib: Some(lib), sources, streams: HashMap::new(), finder_stop }
                }
                None => {
                    log::warn!("NDI runtime not found — NDI sources unavailable (install the NDI runtime to enable them)");
                    NdiClient {
                        lib: None,
                        sources: Arc::new(Mutex::new(Vec::new())),
                        streams: HashMap::new(),
                        finder_stop: Arc::new(AtomicBool::new(false)),
                    }
                }
            }
        }
        pub fn available(&self) -> bool {
            self.lib.is_some()
        }
        pub fn server_names(&self) -> Vec<String> {
            self.sources.lock().unwrap().clone()
        }
        pub fn frame_for(&mut self, source: &str) -> Option<Arc<ScreenFrame>> {
            let lib = self.lib.clone()?;
            if source.is_empty() {
                return None;
            }
            if !self.streams.contains_key(source) {
                self.streams.insert(source.to_string(), spawn_receiver(lib, source.to_string()));
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

    fn cstr_string(p: *const c_char) -> String {
        if p.is_null() {
            return String::new();
        }
        // SAFETY: NDI returns NUL-terminated UTF8 strings.
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }

    /// Discovery label, matching the previous "NAME@url" form (the receiver matches on
    /// the stable NAME before the last '@').
    fn source_label(s: &sys::Source) -> String {
        let name = cstr_string(s.p_ndi_name);
        let url = cstr_string(s.p_url_address);
        if url.is_empty() {
            name
        } else {
            format!("{name}@{url}")
        }
    }

    fn new_find_create() -> sys::FindCreate {
        sys::FindCreate { show_local_sources: true, p_groups: ptr::null(), p_extra_ips: ptr::null() }
    }

    fn spawn_finder(lib: Arc<NdiLib>, sources: Arc<Mutex<Vec<String>>>, stop: Arc<AtomicBool>) {
        thread::Builder::new()
            .name("ndi-finder".into())
            .spawn(move || {
                let create = new_find_create();
                // SAFETY: valid create struct; finder used + freed within this thread.
                let finder = unsafe { (lib.find_create_v2)(&create) };
                if finder.is_null() {
                    return;
                }
                while !stop.load(Ordering::Relaxed) {
                    let mut n: u32 = 0;
                    let arr = unsafe { (lib.find_get_current_sources)(finder, &mut n) };
                    if !arr.is_null() {
                        let names = (0..n as isize)
                            .map(|i| source_label(unsafe { &*arr.offset(i) }))
                            .collect();
                        *sources.lock().unwrap() = names;
                    }
                    thread::sleep(Duration::from_secs(1));
                }
                unsafe { (lib.find_destroy)(finder) };
            })
            .ok();
    }

    fn spawn_receiver(lib: Arc<NdiLib>, source_name: String) -> Stream {
        let frame = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let (frame_t, stop_t) = (frame.clone(), stop.clone());
        // The discovered name is "NAME@url"; match on the stable NAME part.
        let want = source_name.rsplit_once('@').map(|(n, _)| n.to_string()).unwrap_or(source_name);
        thread::Builder::new()
            .name("ndi-recv".into())
            .spawn(move || {
                let create = new_find_create();
                let finder = unsafe { (lib.find_create_v2)(&create) };
                if finder.is_null() {
                    return;
                }
                let mut generation = 0u64;
                while !stop_t.load(Ordering::Relaxed) {
                    // Let discovery populate, then resolve the source by name and copy
                    // its name/url into owned CStrings (the finder array is only valid
                    // until the next find call).
                    unsafe { (lib.find_wait_for_sources)(finder, 2000) };
                    let mut n: u32 = 0;
                    let arr = unsafe { (lib.find_get_current_sources)(finder, &mut n) };
                    let mut found: Option<(CString, CString)> = None;
                    if !arr.is_null() {
                        for i in 0..n as isize {
                            let s = unsafe { &*arr.offset(i) };
                            let label = source_label(s);
                            let nm = label.rsplit_once('@').map(|(x, _)| x).unwrap_or(label.as_str());
                            if nm == want.as_str() {
                                let name = unsafe { CStr::from_ptr(s.p_ndi_name) }.to_owned();
                                let url = if s.p_url_address.is_null() {
                                    CString::default()
                                } else {
                                    unsafe { CStr::from_ptr(s.p_url_address) }.to_owned()
                                };
                                found = Some((name, url));
                                break;
                            }
                        }
                    }
                    let Some((name, url)) = found else {
                        thread::sleep(Duration::from_millis(300));
                        continue;
                    };

                    // Connect — RGBX_RGBA so frames upload straight to an Rgba8 texture.
                    let create = sys::RecvCreateV3 {
                        source_to_connect_to: sys::Source {
                            p_ndi_name: name.as_ptr(),
                            p_url_address: url.as_ptr(),
                        },
                        color_format: sys::COLOR_RGBX_RGBA,
                        bandwidth: sys::BANDWIDTH_HIGHEST,
                        allow_video_fields: true,
                        p_ndi_recv_name: ptr::null(),
                    };
                    // SAFETY: `create` (and the CStrings it points at) outlive this call;
                    // the receiver copies the source internally.
                    let recv = unsafe { (lib.recv_create_v3)(&create) };
                    drop((name, url));
                    if recv.is_null() {
                        thread::sleep(Duration::from_millis(500));
                        continue;
                    }

                    let mut misses = 0u32;
                    while !stop_t.load(Ordering::Relaxed) {
                        let mut video: sys::VideoFrame = unsafe { std::mem::zeroed() };
                        let ft = unsafe {
                            (lib.recv_capture_v3)(recv, &mut video, ptr::null_mut(), ptr::null_mut(), 1000)
                        };
                        match ft {
                            sys::FRAME_TYPE_VIDEO => {
                                misses = 0;
                                let w = video.xres.max(0) as u32;
                                let h = video.yres.max(0) as u32;
                                let stride = if video.line_stride_or_size > 0 {
                                    video.line_stride_or_size as usize
                                } else {
                                    w as usize * 4
                                };
                                let rgba = if video.p_data.is_null() || w == 0 || h == 0 {
                                    Vec::new()
                                } else {
                                    // SAFETY: NDI guarantees `stride * yres` valid bytes.
                                    let data = unsafe {
                                        std::slice::from_raw_parts(video.p_data, stride * h as usize)
                                    };
                                    pack_rgba(data, w, h, stride)
                                };
                                // ALWAYS return the frame to the SDK.
                                unsafe { (lib.recv_free_video_v2)(recv, &video) };
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
                            // Timeout / connecting / error: after several, re-resolve.
                            sys::FRAME_TYPE_NONE | sys::FRAME_TYPE_ERROR => {
                                misses += 1;
                                if misses > 5 {
                                    break;
                                }
                            }
                            // Audio / metadata / status — captured with NULL pointers, so
                            // nothing was filled and nothing to free; ignore.
                            _ => {}
                        }
                    }
                    unsafe { (lib.recv_destroy)(recv) };
                }
                unsafe { (lib.find_destroy)(finder) };
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
