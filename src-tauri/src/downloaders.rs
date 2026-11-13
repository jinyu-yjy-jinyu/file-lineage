//! Third-party downloader task-log adapters.
//!
//! Browsers are not the only thing that puts files in `Downloads`. Tools like
//! Ghost Downloader, Motrix, IDM, FDM or aria2 do not write a
//! `Zone.Identifier` stream and never appear in a browser `History` database,
//! so V2.0.1 could only say "this file is in the download folder" — which
//! answers nothing about *how it was produced*.
//!
//! Most of these tools do keep their own task log. Ghost Downloader writes
//! `%LOCALAPPDATA%\GhostDownloader\tasks.jsonl` (one JSON object per line)
//! with the source URL, output folder, file name, size and timestamps. Reading
//! it read-only gives us the strongest possible evidence, and unlike process
//! attribution it also works for files downloaded **before** the app was
//! installed.
//!
//! Each adapter is isolated in one function so adding another downloader is a
//! small, self-contained change.

use serde_json::Value;

use crate::util::path_key;

pub const GHOST_DOWNLOADER: &str = "Ghost Downloader";

#[derive(Debug, Clone)]
pub struct DownloadRecord {
    pub vendor: String,
    pub name: String,
    /// Full expected path = `outputFolder \ name`.
    pub path: String,
    pub url: String,
    pub referer: String,
    pub size: i64,
    pub created_ms: i64,
    pub completed_ms: i64,
    pub task_id: String,
    pub status: String,
}

impl DownloadRecord {
    pub fn timestamp(&self) -> i64 {
        if self.completed_ms > 0 {
            self.completed_ms
        } else {
            self.created_ms
        }
    }
}

#[derive(Debug, Default)]
pub struct Downloaders {
    records: Vec<DownloadRecord>,
    pub adapter_label: Option<String>,
}

impl Downloaders {
    pub fn load() -> Self {
        let mut out = Downloaders::default();
        let count = out.load_ghost();
        if count > 0 {
            out.adapter_label = Some(format!("Ghost Downloader 任务记录（{} 条）", count));
        }
        out
    }

    fn load_ghost(&mut self) -> usize {
        // `data_local_dir()` is `%LOCALAPPDATA%` on Windows (exactly the old
        // path) and the platform's local app-data root on macOS/Linux.
        let Some(base) = dirs::data_local_dir() else {
            return 0;
        };
        let tasks = base.join("GhostDownloader").join("tasks.jsonl");
        // A busy downloader appends to this file; cap what we read so a huge
        // history can never stall startup.
        let Ok(text) = std::fs::read_to_string(&tasks) else {
            return 0;
        };
        let mut count = 0usize;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || !line.starts_with('{') {
                continue;
            }
            if let Some(record) = parse_ghost_task(line) {
                self.records.push(record);
                count += 1;
            }
            if self.records.len() >= 20_000 {
                break;
            }
        }
        count
    }

    pub fn records(&self) -> &[DownloadRecord] {
        &self.records
    }

    /// Exact path first; then a conservative name+size match restricted to
    /// download-ish folders (covers "downloaded then moved elsewhere").
    pub fn lookup(&self, path: &str, name: &str, size: i64) -> Option<&DownloadRecord> {
        let key = path_key(path);
        if let Some(found) = self
            .records
            .iter()
            .rev()
            .find(|record| path_key(&record.path) == key)
        {
            return Some(found);
        }
        if !path.to_lowercase().contains("download") && !path.to_lowercase().contains("下载") {
            return None;
        }
        self.records.iter().rev().find(|record| {
            record.name.eq_ignore_ascii_case(name)
                && record.size > 0
                && size > 0
                && record.size == size
        })
    }
}

fn parse_ghost_task(line: &str) -> Option<DownloadRecord> {
    let value: Value = serde_json::from_str(line).ok()?;
    let name = value.get("name")?.as_str()?.trim().to_string();
    if name.is_empty() {
        return None;
    }
    let folder = value
        .get("outputFolder")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .trim_end_matches('\\')
        .to_string();
    let path = if folder.is_empty() {
        name.clone()
    } else {
        format!("{}\\{}", folder, name)
    };
    let string_field = |key: &str| -> String {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let number_field = |key: &str| -> i64 {
        value
            .get(key)
            .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
            .unwrap_or(0)
    };
    let referer = value
        .get("headers")
        .and_then(|headers| headers.get("referer").or_else(|| headers.get("referrer")))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    Some(DownloadRecord {
        vendor: GHOST_DOWNLOADER.to_string(),
        name,
        path,
        url: string_field("url"),
        referer,
        size: number_field("fileSize"),
        created_ms: number_field("createdAt") * 1_000,
        completed_ms: number_field("completedAt") * 1_000,
        task_id: string_field("taskId"),
        status: string_field("status"),
    })
}

/// Character-safe truncation for long signed URLs.
pub fn truncate(text: &str, max_chars: usize) -> String {
    let mut short: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        short.push('…');
    }
    short
}
