//! Windows "Recent" shortcut adapter.
//!
//! Every time the shell opens a file (`Enter`, double-click, "Open with"), it
//! drops a `.lnk` into `%APPDATA%\Microsoft\Windows\Recent`. That gives us an
//! *open* signal — "谁打开过它" — which is a different and complementary
//! question from "谁改过它".
//!
//! The link name is the target's display name, so matching is by file name.
//! Because two different files can share a name, the signal is deliberately
//! treated as soft evidence and is labelled as such in the UI text.

use std::collections::HashMap;
#[cfg(windows)]
use std::path::{Path, PathBuf};

#[cfg(windows)]
use crate::util::millis;

#[derive(Debug, Default)]
pub struct Recent {
    /// lowercased file name -> newest `.lnk` modification time
    newest: HashMap<String, i64>,
    /// lowercased file name -> count of distinct shortcuts
    counts: HashMap<String, i64>,
    loaded: bool,
}

impl Recent {
    /// Windows reads the shell's `.lnk` shortcut folder. macOS/Linux have no
    /// equivalent shortcut folder that maps a shortcut name back to a tracked
    /// file, so this degrades to an *empty, not loaded* adapter: the collector
    /// then simply does not advertise an opening-record channel (rather than
    /// advertising one that always answers zero).
    pub fn load() -> Self {
        #[cfg(windows)]
        {
            Self::load_windows()
        }
        #[cfg(not(windows))]
        {
            Recent::default()
        }
    }

    #[cfg(windows)]
    fn load_windows() -> Self {
        let mut recent = Recent::default();
        let Some(dir) = recent_dir() else {
            recent.loaded = true;
            return recent;
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            recent.loaded = true;
            return recent;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e.eq_ignore_ascii_case("lnk")) != Some(true) {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            // Desktop.ini and friends are not user-opened documents.
            if stem.starts_with('.') {
                continue;
            }
            let key = stem.to_lowercase();
            let ts = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .map(millis)
                .unwrap_or(0);
            let slot = recent.newest.entry(key.clone()).or_insert(0);
            if ts > *slot {
                *slot = ts;
            }
            *recent.counts.entry(key).or_insert(0) += 1;
        }
        recent.loaded = true;
        recent
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    pub fn entry_count(&self) -> usize {
        self.newest.len()
    }

    /// Newest shortcut timestamp for a file name, if it was ever opened.
    pub fn newest_for(&self, name: &str) -> Option<i64> {
        self.newest.get(&name.to_lowercase()).copied()
    }

    pub fn shortcut_count(&self, name: &str) -> i64 {
        self.counts.get(&name.to_lowercase()).copied().unwrap_or(0)
    }

    /// `(lowercased name, newest timestamp, shortcut count)` for every entry.
    /// Bounded by the number of shortcuts in the Recent folder.
    pub fn entries(&self) -> Vec<(String, i64, i64)> {
        self.newest
            .iter()
            .map(|(name, ts)| {
                (
                    name.clone(),
                    *ts,
                    self.counts.get(name).copied().unwrap_or(0),
                )
            })
            .collect()
    }
}

#[cfg(windows)]
pub fn recent_dir() -> Option<PathBuf> {
    let appdata = std::env::var("APPDATA").ok()?;
    let dir = Path::new(&appdata)
        .join("Microsoft")
        .join("Windows")
        .join("Recent");
    if dir.is_dir() {
        Some(dir)
    } else {
        None
    }
}
