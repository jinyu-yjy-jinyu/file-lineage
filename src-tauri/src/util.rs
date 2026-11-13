use std::time::{SystemTime, UNIX_EPOCH};

/// Current unix time in milliseconds.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Lowercased, forward-slashed path key used for lookups.
pub fn path_key(p: &str) -> String {
    p.replace('/', "\\").to_lowercase()
}

/// File extension without the dot, lowercased.
pub fn extension_of(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && ext.len() <= 12 => ext.to_lowercase(),
        _ => String::new(),
    }
}

/// Files we never want to track: Office lock files, OS bookkeeping files.
pub fn is_noise_file(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.starts_with("~$")
        || lower == "desktop.ini"
        || lower == "thumbs.db"
        || lower == ".ds_store"
        || lower.ends_with(".tmp")
        || lower.ends_with(".crdownload")
        || lower.ends_with(".part")
        || lower.ends_with(".download")
}

pub fn human_size(bytes: i64) -> String {
    let b = bytes.max(0) as f64;
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b;
    let mut i = 0usize;
    while v >= 1024.0 && i + 1 < UNITS.len() {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{:.1} {}", v, UNITS[i])
    }
}

/// Directories that are never useful to walk inside a user folder.
pub fn should_skip_dir(name: &str) -> bool {
    matches!(
        name,
        "node_modules"
            | ".git"
            | ".svn"
            | "target"
            | "__pycache__"
            | ".venv"
            | "venv"
            | ".cache"
            | "$RECYCLE.BIN"
            | "System Volume Information"
            | "AppData"
    )
}
