//! User-editable configuration.
//!
//! Deliberately a flat `key = value` file rather than JSON: it needs no
//! dependency, it survives hand-editing, and it can carry comments explaining
//! every switch. The file is created with defaults on first run.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Config {
    /// Register `HKCU\...\Run` so the daemon comes back after a reboot.
    pub autostart: bool,
    /// Closing the window hides it instead of exiting.
    pub minimize_to_tray: bool,
    /// Try the ETW kernel trace channel (needs an elevated process).
    pub enable_etw: bool,
    /// Reuse chunk digests when a file only grew (append-only fast path).
    pub incremental_hash: bool,
    /// Events older than this are moved to a read-only per-month archive.
    pub archive_after_days: i64,
    /// Wait this long after the last write before hashing a file, so that
    /// in-progress downloads are not hashed repeatedly.
    pub settle_seconds: i64,
    /// Upper bound on 1 MiB chunks hashed per minute (disk/CPU back-pressure).
    pub hash_chunks_per_minute: i64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            autostart: true,
            minimize_to_tray: true,
            enable_etw: true,
            incremental_hash: true,
            archive_after_days: 90,
            settle_seconds: 20,
            hash_chunks_per_minute: 9_000,
        }
    }
}

const TEMPLATE: &str = r#"# FileLineage V2 configuration
# Values are read once at startup. Delete a line to fall back to its default.

# 开机自启（写入 HKCU\Software\Microsoft\Windows\CurrentVersion\Run）
autostart = true

# 关闭主窗口时最小化到托盘，而不是退出
minimize_to_tray = true

# 尝试启用 ETW 内核事件流（Microsoft-Windows-Kernel-File）。
# 需要以管理员身份运行才会生效；未提权时自动降级为 Restart Manager 句柄归因。
enable_etw = true

# 文件只增长时复用已有分块指纹（追加写入的快速路径）
incremental_hash = true

# 超过多少天的事件转入只读归档库 archive-YYYYMM.db
archive_after_days = 90

# 最后一次写入后静默多少秒才计算指纹（避免给下载中的半成品反复算哈希）
settle_seconds = 20

# 每分钟最多计算多少个 1 MiB 分块（背压上限）
hash_chunks_per_minute = 9000
"#;

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Rewrite a single `key = value` line in place, preserving comments and the
/// rest of the file. Used by the tray menu's autostart toggle.
pub fn write_bool(path: &Path, key: &str, value: bool) -> std::io::Result<()> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut out = String::with_capacity(text.len() + 32);
    let mut replaced = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('#') && !trimmed.starts_with(';') {
            if let Some((existing, _)) = trimmed.split_once('=') {
                if existing.trim().eq_ignore_ascii_case(key) {
                    out.push_str(&format!("{} = {}\n", key, value));
                    replaced = true;
                    continue;
                }
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    if !replaced {
        out.push_str(&format!("\n{} = {}\n", key, value));
    }
    std::fs::write(path, out)
}

impl Config {
    /// Load `config.ini`, creating it with defaults when missing.
    pub fn load(dir: &Path) -> (Self, PathBuf) {
        let path = dir.join("config.ini");
        let mut config = Config::default();

        if !path.is_file() {
            let _ = std::fs::create_dir_all(dir);
            let _ = std::fs::write(&path, TEMPLATE);
            return (config, path);
        }

        if let Ok(text) = std::fs::read_to_string(&path) {
            for raw in text.lines() {
                let line = raw.trim();
                if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                    continue;
                }
                let Some((key, value)) = line.split_once('=') else {
                    continue;
                };
                let key = key.trim().to_ascii_lowercase();
                let value = value.trim();
                match key.as_str() {
                    "autostart" => {
                        if let Some(v) = parse_bool(value) {
                            config.autostart = v;
                        }
                    }
                    "minimize_to_tray" => {
                        if let Some(v) = parse_bool(value) {
                            config.minimize_to_tray = v;
                        }
                    }
                    "enable_etw" => {
                        if let Some(v) = parse_bool(value) {
                            config.enable_etw = v;
                        }
                    }
                    "incremental_hash" => {
                        if let Some(v) = parse_bool(value) {
                            config.incremental_hash = v;
                        }
                    }
                    "archive_after_days" => {
                        if let Ok(v) = value.parse::<i64>() {
                            config.archive_after_days = v.max(1);
                        }
                    }
                    "settle_seconds" => {
                        if let Ok(v) = value.parse::<i64>() {
                            config.settle_seconds = v.clamp(0, 3_600);
                        }
                    }
                    "hash_chunks_per_minute" => {
                        if let Ok(v) = value.parse::<i64>() {
                            config.hash_chunks_per_minute = v.clamp(30, 1_000_000);
                        }
                    }
                    _ => {}
                }
            }
        }
        (config, path)
    }
}
