//! Persistent **library preferences** (#20): the Recent and Favourites
//! pseudo-categories shared by the Library browser and the Shift+A Add menu.
//!
//! Both surfaces index into the same content [`Library`], but indices shift as
//! GDTFs are imported, so we identify an entry by a *stable string key*
//! (`gdtf:<maker>/<name>`, `fixture:<cat>/<name>`, `env:<name>`, `screen:<name>`)
//! rather than a position. Recent is a front-inserted, de-duped, capped list (the
//! Unreal PlacementMode recipe); Favourites is a star-toggled set. Both persist to
//! a small JSON file in the per-user config dir, mirroring `project::recent.json`.

use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::scene::library::Library;

/// How many entries the Recent list keeps (front-insert/de-dupe/cap, per §3 #20).
const RECENT_CAP: usize = 20;

/// Which content vector a key points at — the small tag that, with an index,
/// resolves a stable [`entry_key`] from the live [`Library`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LibItem {
    Gdtf(usize),
    Fixture(usize),
    Env(usize),
    Screen(usize),
}

/// The stable identity string for a library entry, or `None` if the index is out
/// of range (e.g. a GDTF that was removed since the key was recorded).
pub fn entry_key(library: &Library, item: LibItem) -> Option<String> {
    match item {
        LibItem::Gdtf(i) => library
            .gdtf
            .get(i)
            .map(|g| format!("gdtf:{}/{}", g.manufacturer, g.name)),
        LibItem::Fixture(i) => library
            .fixtures
            .get(i)
            .map(|p| format!("fixture:{}/{}", p.category, p.name)),
        LibItem::Env(i) => library.environments.get(i).map(|p| format!("env:{}", p.name)),
        LibItem::Screen(i) => library.screens.get(i).map(|p| format!("screen:{}", p.name)),
    }
}

/// Recent + Favourites, persisted as JSON. Loaded once at startup, saved on every
/// mutation (the lists are tiny, so a synchronous write is fine — same as
/// `project::push_recent`).
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct LibraryPrefs {
    /// Most-recently-added entry keys, front-first, de-duped, capped.
    pub recent: Vec<String>,
    /// Starred entry keys (order irrelevant — rendered in catalog order).
    pub favourites: BTreeSet<String>,
}

impl LibraryPrefs {
    /// Load from disk (config dir); a missing/garbled file yields the default.
    pub fn load() -> Self {
        let Some(p) = prefs_path() else { return Self::default() };
        let Ok(text) = std::fs::read_to_string(&p) else { return Self::default() };
        serde_json::from_str(&text).unwrap_or_default()
    }

    /// Persist to disk (best-effort; a write failure is logged, not fatal).
    fn save(&self) {
        let Some(p) = prefs_path() else { return };
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(&p, text);
        }
    }

    /// Record `key` as just-added: front-insert, de-dupe, cap, then persist.
    pub fn push_recent(&mut self, key: &str) {
        self.recent.retain(|k| k != key);
        self.recent.insert(0, key.to_string());
        self.recent.truncate(RECENT_CAP);
        self.save();
    }

    /// Whether `key` is starred.
    pub fn is_favourite(&self, key: &str) -> bool {
        self.favourites.contains(key)
    }

    /// Star/unstar `key`, then persist. Returns the new state.
    pub fn toggle_favourite(&mut self, key: &str) -> bool {
        let now = if self.favourites.remove(key) {
            false
        } else {
            self.favourites.insert(key.to_string());
            true
        };
        self.save();
        now
    }
}

/// Fuzzy subsequence score of `query` against `hay` (both already lowercased),
/// the shared scorer for the Library browser + Add-menu filters (#20, modelled on
/// Blender's `BLI_string_search`). `None` ⇒ no match (a query char couldn't be
/// found in order). Higher is better: contiguous runs and word-boundary / prefix
/// hits are rewarded so "rb pointe" ranks "Robe Pointe" above an incidental
/// scatter. An empty query matches everything with score 0 (caller keeps catalog
/// order). Pure + allocation-free → cheap to run over the whole catalog per frame.
pub fn fuzzy_score(query: &str, hay: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let hay = hay.as_bytes();
    let q = query.as_bytes();
    let mut score = 0i32;
    let mut qi = 0usize;
    let mut prev_match = false;
    for (hi, &hc) in hay.iter().enumerate() {
        if qi >= q.len() {
            break;
        }
        let boundary = hi == 0 || matches!(hay[hi - 1], b' ' | b'-' | b'_' | b'/' | b'.' | b':');
        if hc == q[qi] {
            score += 1;
            if prev_match {
                score += 4; // contiguous run bonus
            }
            if boundary {
                score += 6; // word-boundary / acronym hit
            }
            if hi == 0 {
                score += 2; // leading-prefix nudge
            }
            qi += 1;
            prev_match = true;
        } else {
            prev_match = false;
        }
    }
    (qi == q.len()).then_some(score)
}

/// `<config>/library.json` — the per-user store, alongside `recent.json`.
fn prefs_path() -> Option<PathBuf> {
    let d = directories::ProjectDirs::from("dev", "Embedder", "previz")?;
    let dir = d.config_dir();
    std::fs::create_dir_all(dir).ok()?;
    Some(dir.join("library.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_front_inserts_dedupes_and_caps() {
        let mut p = LibraryPrefs::default();
        // Front-insert order: last pushed is first.
        p.push_recent("a");
        p.push_recent("b");
        assert_eq!(p.recent, vec!["b".to_string(), "a".to_string()]);
        // Re-adding an existing key moves it to the front (de-dupe, no growth).
        p.push_recent("a");
        assert_eq!(p.recent, vec!["a".to_string(), "b".to_string()]);
        // Cap: pushing more than RECENT_CAP keeps only the most-recent CAP.
        for i in 0..(RECENT_CAP + 5) {
            p.push_recent(&format!("k{i}"));
        }
        assert_eq!(p.recent.len(), RECENT_CAP);
        // The newest key is at the front; the oldest survivors fell off the back.
        assert_eq!(p.recent[0], format!("k{}", RECENT_CAP + 4));
    }

    #[test]
    fn favourite_toggle_round_trips() {
        let mut p = LibraryPrefs::default();
        assert!(!p.is_favourite("x"));
        assert!(p.toggle_favourite("x")); // now starred
        assert!(p.is_favourite("x"));
        assert!(!p.toggle_favourite("x")); // unstarred
        assert!(!p.is_favourite("x"));
    }

    #[test]
    fn fuzzy_matches_subsequence_and_ranks_contiguous_higher() {
        // Empty query matches everything at 0.
        assert_eq!(fuzzy_score("", "anything"), Some(0));
        // Out-of-order chars fail.
        assert!(fuzzy_score("zx", "robe pointe").is_none());
        // Subsequence matches.
        assert!(fuzzy_score("rbpt", "robe pointe").is_some());
        // A contiguous substring outranks a scattered (non-boundary) subsequence
        // of the same query length (both first-match mid-word, so neither gets the
        // prefix/boundary bonus — isolating the contiguity reward).
        let contiguous = fuzzy_score("bcd", "xxbcdxx").unwrap();
        let scattered = fuzzy_score("bcd", "xbxcxdx").unwrap();
        assert!(contiguous > scattered, "{contiguous} !> {scattered}");
        // A word-boundary / prefix hit outranks the same chars mid-word.
        let boundary = fuzzy_score("rob", "robe").unwrap();
        let midword = fuzzy_score("rob", "strobe").unwrap();
        assert!(boundary > midword, "{boundary} !> {midword}");
    }

    #[test]
    fn entry_key_is_stable_and_kind_tagged() {
        let lib = Library::standard();
        // Built-in fixtures always exist; key is kind-tagged + category/name.
        let k = entry_key(&lib, LibItem::Fixture(0)).expect("fixture 0");
        assert!(k.starts_with("fixture:"), "got {k}");
        // Out-of-range index → no key (a removed/garbage entry can't resurrect).
        assert!(entry_key(&lib, LibItem::Gdtf(9999)).is_none());
    }
}
