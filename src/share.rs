//! GDTF Share online fixture library client.
//!
//! Talks to the public GDTF Share API (<https://gdtf-share.com/apis/public/>):
//!   - `login.php`  POST `{user, password}` → sets a session cookie (2 h TTL)
//!   - `getList.php` GET (cookie) → the full revision list (thousands of fixtures)
//!   - `downloadFile.php?rid=N` GET (cookie) → the `.gdtf` archive bytes
//!
//! All network I/O runs on a single worker thread (the UI never blocks). The
//! worker owns a `ureq::Agent`, which keeps the session cookie across requests.
//! Results are published into a shared, mutex-guarded struct; the UI hands them
//! off into its own copy once per frame ([`Share::sync`]) and the worker calls
//! `egui::Context::request_repaint` so an idle UI wakes when data lands.
//!
//! Persistence (via the platform dirs):
//!   - credentials   → config dir `share-credentials.json` (0600 on unix)
//!   - the big list  → cache dir  `share-list.json` (shown instantly on launch)
//!   - downloaded    → data  dir  `gdtf/<rid>.gdtf` (a shared library reused
//!                     across projects; the `rid` is the unique revision id)

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

const BASE: &str = "https://gdtf-share.com/apis/public";

// ---------------------------------------------------------------------------
// API data model
// ---------------------------------------------------------------------------

/// One DMX mode of a fixture revision (from `getList`).
#[derive(Clone, Deserialize)]
pub struct Mode {
    #[serde(default)]
    pub name: String,
    #[serde(default, deserialize_with = "de_i64")]
    pub dmxfootprint: i64,
}

/// One fixture revision in the GDTF Share list.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListEntry {
    #[serde(default, deserialize_with = "de_i64")]
    pub rid: i64,
    #[serde(default)]
    pub fixture: String,
    #[serde(default)]
    pub manufacturer: String,
    #[serde(default)]
    pub revision: String,
    #[serde(default, deserialize_with = "de_i64")]
    pub creation_date: i64,
    #[serde(default, deserialize_with = "de_i64")]
    pub last_modified: i64,
    #[serde(default)]
    pub uploader: String,
    #[serde(default, deserialize_with = "de_f64")]
    pub rating: f64,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub creator: String,
    #[serde(default)]
    pub uuid: String,
    #[serde(default, deserialize_with = "de_i64")]
    pub filesize: i64,
    #[serde(default)]
    pub modes: Vec<Mode>,
}

#[derive(Deserialize)]
struct ListResp {
    #[serde(default)]
    result: bool,
    #[serde(default)]
    timestamp: i64,
    #[serde(default)]
    list: Vec<ListEntry>,
    #[serde(default)]
    error: String,
}

#[derive(Deserialize)]
struct LoginResp {
    #[serde(default)]
    result: bool,
    #[serde(default)]
    notice: String,
    #[serde(default)]
    error: String,
}

#[derive(Deserialize)]
struct ErrResp {
    #[serde(default)]
    error: String,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Credentials {
    user: String,
    password: String,
}

// Some GDTF Share fields come back as numbers in one deployment and quoted
// strings in another — accept both so the client doesn't break on a server tweak.
fn de_i64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Loose {
        I(i64),
        F(f64),
        S(String),
        None,
    }
    Ok(match Loose::deserialize(d)? {
        Loose::I(i) => i,
        Loose::F(f) => f as i64,
        Loose::S(s) => s.trim().parse().unwrap_or(0),
        Loose::None => 0,
    })
}

fn de_f64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Loose {
        F(f64),
        S(String),
        None,
    }
    Ok(match Loose::deserialize(d)? {
        Loose::F(f) => f,
        Loose::S(s) => s.trim().parse().unwrap_or(0.0),
        Loose::None => 0.0,
    })
}

// ---------------------------------------------------------------------------
// on-disk locations
// ---------------------------------------------------------------------------

fn project_dirs() -> Option<directories::ProjectDirs> {
    directories::ProjectDirs::from("dev", "Embedder", "previz")
}

/// The shared directory that holds every downloaded `.gdtf` (reused across
/// projects). Created on first use; falls back to a temp dir if no home.
pub fn gdtf_dir() -> PathBuf {
    let dir = project_dirs()
        .map(|d| d.data_dir().join("gdtf"))
        .unwrap_or_else(|| std::env::temp_dir().join("previz-gdtf"));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn credentials_path() -> Option<PathBuf> {
    project_dirs().map(|d| d.config_dir().join("share-credentials.json"))
}

fn list_cache_path() -> Option<PathBuf> {
    project_dirs().map(|d| d.cache_dir().join("share-list.json"))
}

fn load_credentials() -> Option<Credentials> {
    let s = std::fs::read_to_string(credentials_path()?).ok()?;
    serde_json::from_str(&s).ok()
}

fn save_credentials(c: &Credentials) {
    let Some(path) = credentials_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(bytes) = serde_json::to_vec_pretty(c) {
        let _ = std::fs::write(&path, bytes);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
    }
}

fn delete_credentials() {
    if let Some(path) = credentials_path() {
        let _ = std::fs::remove_file(path);
    }
}

fn load_cached_list() -> Option<(Vec<ListEntry>, i64)> {
    let s = std::fs::read_to_string(list_cache_path()?).ok()?;
    let r: ListResp = serde_json::from_str(&s).ok()?;
    r.result.then_some((r.list, r.timestamp))
}

fn save_cached_list(raw: &str) {
    let Some(path) = list_cache_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, raw);
}

/// The revision ids already present in the shared dir (`<rid>.gdtf`).
fn scan_downloaded(dir: &Path) -> HashSet<i64> {
    let mut out = HashSet::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some("gdtf")
                && let Some(rid) = p.file_stem().and_then(|s| s.to_str()).and_then(|s| s.parse::<i64>().ok())
            {
                out.insert(rid);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// worker thread
// ---------------------------------------------------------------------------

enum Cmd {
    Login { user: String, password: String, save: bool },
    Logout,
    FetchList,
    Download(i64),
}

/// State the worker publishes and the UI reads (briefly locked).
#[derive(Default)]
struct Shared {
    logged_in: bool,
    busy: Option<String>,
    error: Option<String>,
    notice: Option<String>,
    downloaded: HashSet<i64>,
    downloading: HashSet<i64>,
    /// A freshly fetched list waiting for the UI to take ownership.
    new_list: Option<(Vec<ListEntry>, i64)>,
    list_fetched: Option<SystemTime>,
}

fn build_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(120))
        .build()
}

fn update<F: FnOnce(&mut Shared)>(shared: &Arc<Mutex<Shared>>, ctx: &egui::Context, f: F) {
    if let Ok(mut g) = shared.lock() {
        f(&mut g);
    }
    ctx.request_repaint();
}

/// Read a response body to a string even when ureq treats a non-2xx as an error
/// (the API returns its `{result:false,error}` JSON with a 4xx status).
fn body_text(result: Result<ureq::Response, ureq::Error>) -> Result<String, String> {
    match result {
        Ok(resp) => resp.into_string().map_err(|e| e.to_string()),
        Err(ureq::Error::Status(_, resp)) => resp.into_string().map_err(|e| e.to_string()),
        Err(ureq::Error::Transport(t)) => Err(t.to_string()),
    }
}

fn error_message(body: &str) -> String {
    serde_json::from_str::<ErrResp>(body)
        .ok()
        .map(|e| e.error)
        .filter(|e| !e.is_empty())
        .unwrap_or_else(|| "unexpected server response".into())
}

/// Pull every `Set-Cookie` from a response into a single `name=value; name=value`
/// `Cookie` header value (the GDTF Share session, typically `PHPSESSID`).
fn collect_session_cookie(resp: &ureq::Response) -> Option<String> {
    let parts: Vec<String> = resp
        .all("set-cookie")
        .into_iter()
        .filter_map(|sc| sc.split(';').next())
        .map(|kv| kv.trim().to_string())
        .filter(|kv| kv.contains('=') && !kv.starts_with('='))
        .collect();
    (!parts.is_empty()).then(|| parts.join("; "))
}

/// Returns `(notice, session-cookie)` on success. We read the cookie off the
/// login response and attach it to later requests ourselves.
fn do_login(agent: &ureq::Agent, user: &str, password: &str) -> Result<(String, Option<String>), String> {
    let resp = match agent
        .post(&format!("{BASE}/login.php"))
        .send_json(serde_json::json!({ "user": user, "password": password }))
    {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(ureq::Error::Transport(t)) => return Err(t.to_string()),
    };
    let cookie = collect_session_cookie(&resp);
    let body = resp.into_string().map_err(|e| e.to_string())?;
    let r: LoginResp = serde_json::from_str(&body).map_err(|e| format!("login parse: {e}"))?;
    if r.result {
        if cookie.is_none() {
            log::warn!("gdtf-share: login succeeded but no session cookie was returned");
        }
        Ok((if r.notice.is_empty() { "Signed in".into() } else { r.notice }, cookie))
    } else {
        Err(if r.error.is_empty() { "invalid user or password".into() } else { r.error })
    }
}

fn with_cookie(req: ureq::Request, cookie: &Option<String>) -> ureq::Request {
    match cookie {
        Some(c) => req.set("Cookie", c),
        None => req,
    }
}

fn do_fetch(agent: &ureq::Agent, shared: &Arc<Mutex<Shared>>, ctx: &egui::Context, cookie: &Option<String>) {
    update(shared, ctx, |s| {
        s.busy = Some("Loading library…".into());
        s.error = None;
    });
    let result = (|| -> Result<(Vec<ListEntry>, i64, String), String> {
        let body = body_text(with_cookie(agent.get(&format!("{BASE}/getList.php")), cookie).call())?;
        let r: ListResp = serde_json::from_str(&body).map_err(|e| format!("list parse: {e}"))?;
        if !r.result {
            return Err(if r.error.is_empty() { "could not load the list".into() } else { r.error });
        }
        Ok((r.list, r.timestamp, body))
    })();
    match result {
        Ok((list, ts, raw)) => {
            save_cached_list(&raw);
            update(shared, ctx, |s| {
                s.new_list = Some((list, ts));
                s.list_fetched = Some(SystemTime::now());
                s.busy = None;
            });
        }
        Err(e) => update(shared, ctx, |s| {
            s.error = Some(e);
            s.busy = None;
        }),
    }
}

fn do_download(agent: &ureq::Agent, rid: i64, dir: &Path, cookie: &Option<String>) -> Result<(), String> {
    let bytes = match with_cookie(agent.get(&format!("{BASE}/downloadFile.php?rid={rid}")), cookie).call() {
        Ok(resp) => {
            let mut v = Vec::new();
            resp.into_reader().read_to_end(&mut v).map_err(|e| e.to_string())?;
            v
        }
        Err(ureq::Error::Status(_, resp)) => {
            return Err(error_message(&resp.into_string().unwrap_or_default()));
        }
        Err(ureq::Error::Transport(t)) => return Err(t.to_string()),
    };
    // A GDTF file is a ZIP (magic "PK"). Anything else is an error payload.
    if bytes.len() < 4 || &bytes[..2] != b"PK" {
        return Err(error_message(&String::from_utf8_lossy(&bytes)));
    }
    // Write to a temp file then rename, so a half-written download is never
    // mistaken for a complete cached fixture.
    let tmp = dir.join(format!(".{rid}.part"));
    std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dir.join(format!("{rid}.gdtf"))).map_err(|e| e.to_string())?;
    Ok(())
}

fn run_worker(rx: Receiver<Cmd>, shared: Arc<Mutex<Shared>>, dir: PathBuf, ctx: egui::Context) {
    let agent = build_agent();
    // The GDTF Share session cookie captured at login and attached to every later
    // request (manual, so it survives redirects / jar quirks).
    let mut cookie: Option<String> = None;
    while let Ok(cmd) = rx.recv() {
        match cmd {
            Cmd::Login { user, password, save } => {
                update(&shared, &ctx, |s| {
                    s.busy = Some("Signing in…".into());
                    s.error = None;
                });
                match do_login(&agent, &user, &password) {
                    Ok((notice, ck)) => {
                        cookie = ck;
                        let online = cookie.is_some();
                        if save {
                            save_credentials(&Credentials { user, password });
                        } else {
                            delete_credentials();
                        }
                        let need_fetch = shared.lock().map(|g| g.new_list.is_none()).unwrap_or(true);
                        update(&shared, &ctx, |s| {
                            s.logged_in = online;
                            s.notice = Some(notice);
                            s.busy = None;
                            if !online {
                                s.error = Some("signed in but the server sent no session cookie".into());
                            }
                        });
                        // First sign-in with no cached list: pull it automatically.
                        if online && need_fetch && list_cache_path().map(|p| !p.exists()).unwrap_or(true) {
                            do_fetch(&agent, &shared, &ctx, &cookie);
                        }
                    }
                    Err(e) => update(&shared, &ctx, |s| {
                        s.logged_in = false;
                        s.error = Some(e);
                        s.busy = None;
                    }),
                }
            }
            Cmd::Logout => {
                cookie = None;
                delete_credentials();
                update(&shared, &ctx, |s| {
                    s.logged_in = false;
                    s.notice = None;
                    s.error = None;
                });
            }
            Cmd::FetchList => {
                if cookie.is_some() {
                    do_fetch(&agent, &shared, &ctx, &cookie);
                } else {
                    update(&shared, &ctx, |s| s.error = Some("sign in to refresh the library".into()));
                }
            }
            Cmd::Download(rid) => {
                update(&shared, &ctx, |s| {
                    s.downloading.insert(rid);
                    s.error = None;
                });
                let r = do_download(&agent, rid, &dir, &cookie);
                update(&shared, &ctx, |s| {
                    s.downloading.remove(&rid);
                    match r {
                        Ok(()) => {
                            s.downloaded.insert(rid);
                        }
                        Err(e) => s.error = Some(format!("download failed: {e}")),
                    }
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// UI-facing handle
// ---------------------------------------------------------------------------

/// The GDTF Share handle the UI owns. Network state lives behind a worker; this
/// struct also holds the UI-side copies (synced once per frame) and the login /
/// filter form fields.
pub struct Share {
    shared: Arc<Mutex<Shared>>,
    tx: Option<Sender<Cmd>>,
    _worker: Option<JoinHandle<()>>,
    started: bool,
    gdtf_dir: PathBuf,
    had_saved_creds: bool,

    // synced copies (read freely during render — no lock held)
    pub list: Vec<ListEntry>,
    pub list_timestamp: i64,
    pub list_fetched: Option<SystemTime>,
    pub logged_in: bool,
    pub busy: Option<String>,
    pub error: Option<String>,
    pub notice: Option<String>,
    pub downloaded: HashSet<i64>,
    pub downloading: HashSet<i64>,

    // login form + filters (UI state)
    pub user: String,
    pub password: String,
    pub remember: bool,
    pub search: String,
    pub manufacturer: String,
    pub downloaded_only: bool,
    pub updates_only: bool,
    /// rids the user asked to add — downloaded in the background, then imported +
    /// placed on the main thread once the file lands.
    pub pending_add: HashSet<i64>,

    // derived view, rebuilt only when the inputs change (cheap with 1000s of rows)
    filtered: Vec<usize>,
    mfr_list: Vec<String>,
    uuid_max_rid: HashMap<String, i64>,
    uuid_dl_max: HashMap<String, i64>,
    view_sig: Option<u64>,
}

/// Per-row state shown in the library list.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RowStatus {
    /// Not in the shared dir.
    Cloud,
    /// This exact revision is downloaded.
    Cached,
    /// A newer revision than the one downloaded is available.
    Update,
    /// A download is in flight.
    Downloading,
}

impl Share {
    pub fn new() -> Self {
        let gdtf_dir = gdtf_dir();
        let downloaded = scan_downloaded(&gdtf_dir);
        let cached = load_cached_list();
        let creds = load_credentials();
        let had_saved_creds = creds.is_some();
        let creds = creds.unwrap_or_default();
        Self {
            shared: Arc::new(Mutex::new(Shared { downloaded: downloaded.clone(), ..Default::default() })),
            tx: None,
            _worker: None,
            started: false,
            gdtf_dir,
            had_saved_creds,
            list: cached.as_ref().map(|(l, _)| l.clone()).unwrap_or_default(),
            list_timestamp: cached.as_ref().map(|(_, t)| *t).unwrap_or(0),
            list_fetched: None,
            logged_in: false,
            busy: None,
            error: None,
            notice: None,
            downloaded,
            downloading: HashSet::new(),
            user: creds.user,
            password: creds.password,
            remember: had_saved_creds,
            search: String::new(),
            manufacturer: String::new(),
            downloaded_only: false,
            updates_only: false,
            pending_add: HashSet::new(),
            filtered: Vec::new(),
            mfr_list: Vec::new(),
            uuid_max_rid: HashMap::new(),
            uuid_dl_max: HashMap::new(),
            view_sig: None,
        }
    }

    /// Rebuild the filtered/sorted index, the manufacturer dropdown, and the
    /// per-uuid revision maps — only when an input actually changed.
    pub fn ensure_view(&mut self) {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.list_timestamp.hash(&mut h);
        self.list.len().hash(&mut h);
        // Sort the (small) download sets so the hash is order-independent but still
        // collision-resistant — a HashSet's iteration order isn't stable.
        let mut dl: Vec<i64> = self.downloaded.iter().copied().collect();
        dl.sort_unstable();
        dl.hash(&mut h);
        let mut ing: Vec<i64> = self.downloading.iter().copied().collect();
        ing.sort_unstable();
        ing.hash(&mut h);
        self.search.to_lowercase().hash(&mut h);
        self.manufacturer.hash(&mut h);
        self.downloaded_only.hash(&mut h);
        self.updates_only.hash(&mut h);
        let sig = h.finish();
        if self.view_sig == Some(sig) {
            return;
        }
        self.view_sig = Some(sig);

        // manufacturer dropdown
        let mut mfrs: Vec<String> = self.list.iter().map(|e| e.manufacturer.clone()).collect();
        mfrs.sort_unstable_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));
        mfrs.dedup();
        mfrs.retain(|m| !m.is_empty());
        self.mfr_list = mfrs;

        // per-uuid: newest revision in the list, and newest downloaded revision
        self.uuid_max_rid.clear();
        self.uuid_dl_max.clear();
        for e in &self.list {
            if e.uuid.is_empty() {
                continue;
            }
            let cur = self.uuid_max_rid.entry(e.uuid.clone()).or_insert(e.rid);
            if e.rid > *cur {
                *cur = e.rid;
            }
            if self.downloaded.contains(&e.rid) {
                let d = self.uuid_dl_max.entry(e.uuid.clone()).or_insert(e.rid);
                if e.rid > *d {
                    *d = e.rid;
                }
            }
        }

        // filtered + sorted index
        let search = self.search.to_lowercase();
        let mfr = &self.manufacturer;
        let mut idx: Vec<usize> = self
            .list
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                if !mfr.is_empty() && &e.manufacturer != mfr {
                    return false;
                }
                if !search.is_empty()
                    && !e.fixture.to_lowercase().contains(&search)
                    && !e.manufacturer.to_lowercase().contains(&search)
                {
                    return false;
                }
                if self.downloaded_only && !self.downloaded.contains(&e.rid) {
                    return false;
                }
                if self.updates_only && self.status_of(e) != RowStatus::Update {
                    return false;
                }
                true
            })
            .map(|(i, _)| i)
            .collect();
        idx.sort_by(|&a, &b| {
            let (ea, eb) = (&self.list[a], &self.list[b]);
            ea.manufacturer
                .to_lowercase()
                .cmp(&eb.manufacturer.to_lowercase())
                .then_with(|| ea.fixture.to_lowercase().cmp(&eb.fixture.to_lowercase()))
        });
        self.filtered = idx;
    }

    fn status_of(&self, e: &ListEntry) -> RowStatus {
        if self.downloading.contains(&e.rid) {
            RowStatus::Downloading
        } else if self.downloaded.contains(&e.rid) {
            RowStatus::Cached
        } else if self.uuid_max_rid.get(&e.uuid) == Some(&e.rid) && self.uuid_dl_max.contains_key(&e.uuid) {
            RowStatus::Update
        } else {
            RowStatus::Cloud
        }
    }

    /// Per-row status for the entry at list index `i`.
    pub fn status(&self, i: usize) -> RowStatus {
        self.list.get(i).map(|e| self.status_of(e)).unwrap_or(RowStatus::Cloud)
    }

    /// Filtered/sorted indices into `list` (built by [`ensure_view`]).
    pub fn filtered(&self) -> &[usize] {
        &self.filtered
    }

    /// Sorted unique manufacturers for the filter dropdown.
    pub fn manufacturers(&self) -> &[String] {
        &self.mfr_list
    }

    /// Start the worker on first use, passing the egui context so background
    /// results can wake the UI. Auto-signs-in when credentials were saved.
    pub fn ensure_started(&mut self, ctx: &egui::Context) {
        if self.started {
            return;
        }
        self.started = true;
        let (tx, rx) = channel();
        let shared = self.shared.clone();
        let dir = self.gdtf_dir.clone();
        let ctx = ctx.clone();
        self._worker = Some(std::thread::spawn(move || run_worker(rx, shared, dir, ctx)));
        self.tx = Some(tx);
        if self.had_saved_creds && !self.user.is_empty() {
            self.login();
        }
    }

    /// Pull the latest worker state into the UI-side copies. Cheap; call once a
    /// frame while the window is open.
    pub fn sync(&mut self) {
        let Ok(mut g) = self.shared.lock() else { return };
        if let Some((list, ts)) = g.new_list.take() {
            self.list = list;
            self.list_timestamp = ts;
        }
        self.logged_in = g.logged_in;
        self.busy = g.busy.clone();
        self.error = g.error.clone();
        if let Some(n) = g.notice.take() {
            self.notice = Some(n);
        }
        self.downloaded = g.downloaded.clone();
        self.downloading = g.downloading.clone();
        if g.list_fetched.is_some() {
            self.list_fetched = g.list_fetched;
        }
    }

    fn send(&self, cmd: Cmd) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(cmd);
        }
    }

    pub fn login(&self) {
        self.send(Cmd::Login {
            user: self.user.clone(),
            password: self.password.clone(),
            save: self.remember,
        });
    }

    pub fn logout(&mut self) {
        self.send(Cmd::Logout);
        self.logged_in = false;
    }

    pub fn refresh(&self) {
        self.send(Cmd::FetchList);
    }

    pub fn download(&self, rid: i64) {
        self.send(Cmd::Download(rid));
    }

    pub fn gdtf_path(&self, rid: i64) -> PathBuf {
        self.gdtf_dir.join(format!("{rid}.gdtf"))
    }

    pub fn is_busy(&self) -> bool {
        self.busy.is_some()
    }

    /// Inject demo data so the catalogue can be screenshotted headlessly without
    /// real credentials (used only by the PREVIZ_UI screenshot path).
    pub fn debug_demo(&mut self) {
        self.logged_in = true;
        let mk = |rid: i64, mfr: &str, fix: &str, rev: &str, rating: f64, modes: &[(&str, i64)], size: i64, uuid: &str| ListEntry {
            rid,
            fixture: fix.into(),
            manufacturer: mfr.into(),
            revision: rev.into(),
            creation_date: 0,
            last_modified: 0,
            uploader: "Manuf.".into(),
            rating,
            version: "1.2".into(),
            creator: "demo".into(),
            uuid: uuid.into(),
            filesize: size,
            modes: modes.iter().map(|(n, f)| Mode { name: (*n).into(), dmxfootprint: *f }).collect(),
        };
        self.list = vec![
            mk(1001, "Robe", "Robin MegaPointe", "rev. 3", 4.6, &[("Mode 1", 41), ("Mode 2", 35)], 4_809_117, "u-mega"),
            mk(1002, "Robe", "Robin Esprite", "rev. 2", 4.4, &[("Standard", 49)], 5_220_410, "u-esprite"),
            mk(1003, "Martin", "MAC Aura PXL", "rev. 1", 4.2, &[("Basic", 20), ("Extended", 96)], 3_110_002, "u-aura"),
            mk(1004, "Ayrton", "Khamsin S", "rev. 4", 4.8, &[("Mode A", 44)], 6_004_120, "u-khamsin"),
            mk(1005, "Chauvet", "Maverick Storm 1", "rev. 1", 3.9, &[("16-bit", 38)], 2_551_900, "u-maverick"),
            mk(1006, "Clay Paky", "Sharpy X Frame", "rev. 2", 4.5, &[("Standard", 60)], 7_220_004, "u-sharpy"),
        ];
        self.list_timestamp = 1_700_000_000;
        self.downloaded.insert(1002); // show a "cached" row
        self.uuid_dl_max.clear(); // force ensure_view to recompute status
        self.view_sig = None;
        // Mirror into the shared state so `sync()` doesn't clobber the demo.
        if let Ok(mut g) = self.shared.lock() {
            g.logged_in = true;
            g.downloaded.insert(1002);
        }
    }
}

impl Default for Share {
    fn default() -> Self {
        Self::new()
    }
}
