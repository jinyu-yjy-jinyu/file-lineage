use std::time::{SystemTime, UNIX_EPOCH};

/// Current unix time in milliseconds.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Normalized path key used for lookups.
///
/// Windows filesystems are case-insensitive, so the key is lowercased. Unix
/// filesystems are case-sensitive (macOS is a special case, but its source
/// records use the on-disk spelling), so the original case is preserved there
/// and only the separators are normalized — otherwise `/Users/A/File.txt` and
/// `/users/a/file.txt` would be wrongly treated as the same file.
#[cfg(windows)]
pub fn path_key(p: &str) -> String {
    p.replace('/', "\\").to_lowercase()
}

#[cfg(not(windows))]
pub fn path_key(p: &str) -> String {
    p.replace('\\', "/")
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
    // Page/hibernation files are huge, permanently locked and change on every
    // boot; indexing them only produces noise (exact match, not a suffix rule).
    if matches!(
        lower.as_str(),
        "pagefile.sys" | "swapfile.sys" | "hiberfil.sys" | "memory.dmp"
    ) {
        return true;
    }
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
            // Filesystem bookkeeping that appears when the walk starts at a
            // drive/root instead of a user folder. They are not user data and
            // some are not even readable without elevated rights.
            | ".fseventsd"
            | ".Spotlight-V100"
            | ".Trashes"
            | "lost+found"
    )
}

/// NUL-terminated UTF-16 for Win32 `*W` entry points.
///
/// Windows-only: `std::os::windows` does not exist on other targets, so both
/// helpers are compiled out entirely there. Their only callers (`win32.rs`
/// users) are Windows-gated too.
#[cfg(windows)]
pub fn to_wide(value: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    value.encode_wide().chain(std::iter::once(0)).collect()
}

/// Same, but with the `\\?\` prefix that lifts the 260-character MAX_PATH
/// limit and disables path normalization (dot segments, trailing spaces).
#[cfg(windows)]
pub fn extended_path(path: &std::path::Path) -> Vec<u16> {
    let text = path.to_string_lossy();
    if text.starts_with(r"\\") {
        to_wide(path.as_os_str())
    } else {
        to_wide(std::ffi::OsStr::new(&format!(r"\\?\{}", text)))
    }
}

/// Milliseconds since the Unix epoch for a `SystemTime`.
pub fn millis(time: std::time::SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Civil date for days since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    (year, month, day)
}

/// Human-readable UTC timestamp for unix milliseconds.
pub fn format_ms(ms: i64) -> String {
    const DAY: i64 = 86_400_000;
    let days = ms.div_euclid(DAY);
    let rest = ms.rem_euclid(DAY);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
        year,
        month,
        day,
        rest / 3_600_000,
        (rest % 3_600_000) / 60_000,
        (rest % 60_000) / 1_000
    )
}

/// UTC month bucket for a timestamp: `(start_ms, end_ms_exclusive, "YYYYMM")`.
/// Used by the cold-storage archiver.
pub fn month_bounds_ms(ts_ms: i64) -> (i64, i64, String) {
    const DAY: i64 = 86_400_000;
    let (year, month, _) = civil_from_days(ts_ms.div_euclid(DAY));
    let start = days_from_civil(year, month, 1) * DAY;
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let end = days_from_civil(next_year, next_month, 1) * DAY;
    (start, end, format!("{:04}{:02}", year, month))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_system_noise_names_are_skipped() {
        for name in [
            "pagefile.sys",
            "PAGEFILE.SYS",
            "swapfile.sys",
            "hiberfil.sys",
            "memory.dmp",
        ] {
            assert!(is_noise_file(name), "{name} should be noise");
        }
        assert!(!is_noise_file("my-pagefile.sys"));
    }

    #[test]
    fn root_level_bookkeeping_dirs_are_skipped() {
        for name in [".fseventsd", ".Spotlight-V100", ".Trashes", "lost+found"] {
            assert!(should_skip_dir(name), "{name} should be skipped");
        }
    }
}
