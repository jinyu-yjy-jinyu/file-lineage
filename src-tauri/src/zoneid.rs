//! The `Zone.Identifier` alternate data stream.
//!
//! When a browser (or any shell-aware downloader) writes a file that came from
//! the internet, Windows attaches a hidden stream:
//!
//! ```text
//! [ZoneTransfer]
//! ZoneId=3
//! ReferrerUrl=https://example.com/page
//! HostUrl=https://cdn.example.com/file.zip
//! ```
//!
//! This is the single most durable source evidence on a Windows machine: it
//! survives clearing browser history, it is not Chrome/Edge specific, and it
//! points at the *actual* origin host. V2 reads it before any browser database.

use std::path::Path;

pub const STREAM_SUFFIX: &str = ":Zone.Identifier";

#[derive(Debug, Clone, Default)]
pub struct ZoneInfo {
    /// 0 = local machine, 1 = intranet, 2 = trusted, 3 = internet, 4 = restricted.
    pub zone_id: i64,
    pub referrer: String,
    pub host: String,
}

impl ZoneInfo {
    pub fn zone_label(&self) -> &'static str {
        match self.zone_id {
            0 => "本机",
            1 => "本地 Intranet",
            2 => "受信任站点",
            3 => "Internet",
            4 => "受限站点",
            _ => "未知区域",
        }
    }

    /// Prefer the concrete download host; fall back to the referring page.
    pub fn origin(&self) -> &str {
        if !self.host.is_empty() {
            &self.host
        } else {
            &self.referrer
        }
    }
}

fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(key) {
            if let Some(value) = rest.strip_prefix('=') {
                let value = value.trim();
                if !value.is_empty() {
                    return Some(value);
                }
            }
        }
    }
    None
}

/// Read the stream, or `None` when the file has no zone marker.
pub fn read(path: &str) -> Option<ZoneInfo> {
    read_stream(Path::new(&format!("{}{}", path, STREAM_SUFFIX)))
}

/// Read an arbitrary stream path (exposed for tests and for callers that
/// already built the full `file:Stream` string).
pub fn read_stream(stream_path: &Path) -> Option<ZoneInfo> {
    let bytes = std::fs::read(stream_path).ok()?;
    if bytes.is_empty() || bytes.len() > 64 * 1024 {
        return None;
    }
    let text = String::from_utf8_lossy(&bytes);
    if !text.contains("Zone") && !text.contains("Referrer") {
        return None;
    }
    let mut info = ZoneInfo::default();
    if let Some(value) = field(&text, "ZoneId") {
        info.zone_id = value.parse::<i64>().unwrap_or(-1);
    }
    info.referrer = field(&text, "ReferrerUrl").unwrap_or("").to_string();
    info.host = field(&text, "HostUrl").unwrap_or("").to_string();
    if info.host.is_empty() && info.referrer.is_empty() && info.zone_id < 0 {
        return None;
    }
    Some(info)
}
