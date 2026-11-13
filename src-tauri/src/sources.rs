//! Source adapters for Q1 ("这个文件是哪来的").
//!
//! V1 adapters are deliberately read-only and side-effect free:
//!   * browser download records — Chrome / Edge / Brave / Vivaldi / Chromium
//!     `History` SQLite, Firefox `places.sqlite` (copied to a temp file first,
//!     because browsers keep the live DB locked);
//!   * path-shape heuristics for mail attachments and IM receive folders.
//!
//! Every adapter is isolated in one function so V2 can add Outlook / Slack /
//! WeChat *content* parsers without touching the collector.

use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use rusqlite::{Connection, OpenFlags};

use crate::util::path_key;
use crate::SourceInfo;

/// Chrome/Edge/Firefox timestamps: microseconds since 1601-01-01.
const CHROME_EPOCH_OFFSET_MS: i64 = 11_644_473_600_000;

fn chrome_time_to_ms(raw: i64) -> i64 {
    if raw <= 0 {
        0
    } else {
        raw / 1000 - CHROME_EPOCH_OFFSET_MS
    }
}

#[derive(Debug, Clone)]
struct Download {
    path_key: String,
    name_key: String,
    site: String,
    tab_url: String,
    browser: String,
    start_ms: i64,
    bytes: i64,
}

#[derive(Debug, Default)]
pub struct Sources {
    downloads: Vec<Download>,
    adapters: Vec<String>,
}

impl Sources {
    pub fn load() -> Self {
        let mut this = Sources::default();
        this.load_chromium();
        this.load_firefox();
        this
    }

    pub fn adapter_names(&self) -> &[String] {
        &self.adapters
    }

    pub fn record_count(&self) -> usize {
        self.downloads.len()
    }

    fn load_chromium(&mut self) {
        let local = match std::env::var("LOCALAPPDATA") {
            Ok(v) => PathBuf::from(v),
            Err(_) => return,
        };
        let roots: [(&str, &str); 5] = [
            ("Chrome", r"Google\Chrome\User Data"),
            ("Edge", r"Microsoft\Edge\User Data"),
            ("Brave", r"BraveSoftware\Brave-Browser\User Data"),
            ("Vivaldi", r"Vivaldi\User Data"),
            ("Chromium", r"Chromium\User Data"),
        ];

        for (browser, rel) in roots {
            let root = local.join(rel);
            if !root.is_dir() {
                continue;
            }
            let mut found = 0usize;
            let entries = match fs::read_dir(&root) {
                Ok(e) => e,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let history = entry.path().join("History");
                if !history.is_file() {
                    continue;
                }
                let n = read_chromium_history(&history, browser, &mut self.downloads);
                found += n;
            }
            if found > 0 {
                self.adapters
                    .push(format!("{} ({} 条下载记录)", browser, found));
            }
        }
    }

    fn load_firefox(&mut self) {
        let roaming = match std::env::var("APPDATA") {
            Ok(v) => PathBuf::from(v),
            Err(_) => return,
        };
        let root = roaming.join("Mozilla").join("Firefox").join("Profiles");
        let entries = match fs::read_dir(root) {
            Ok(e) => e,
            Err(_) => return,
        };
        let mut found = 0usize;
        for entry in entries.flatten() {
            let places = entry.path().join("places.sqlite");
            if places.is_file() {
                found += read_firefox_places(&places, &mut self.downloads);
            }
        }
        if found > 0 {
            self.adapters
                .push(format!("Firefox ({} 条下载记录)", found));
        }
    }

    /// Exact path match first, then a conservative name+size match inside a
    /// Downloads folder (covers "downloaded then moved to Desktop").
    pub fn lookup(&self, path: &str, name: &str, size: i64) -> Option<SourceInfo> {
        let key = path_key(path);
        if let Some(rec) = self.downloads.iter().find(|d| d.path_key == key) {
            return Some(to_source_info(rec));
        }
        let lname = name.to_lowercase();
        if let Some(rec) = self
            .downloads
            .iter()
            .find(|d| d.name_key == lname && (d.bytes <= 0 || size <= 0 || d.bytes == size))
        {
            return Some(to_source_info(rec));
        }
        None
    }
}

fn to_source_info(rec: &Download) -> SourceInfo {
    let site = clean_url(&rec.site);
    let page = clean_url(&rec.tab_url);
    let headline = if site.is_empty() {
        format!("{} 下载", rec.browser)
    } else {
        format!("{} 下载 · {}", rec.browser, site)
    };
    let mut parts = Vec::new();
    if !page.is_empty() && page != site {
        parts.push(format!("来源页面：{}", page));
    }
    if rec.start_ms > 0 {
        parts.push(format!("下载时间：{} (Unix ms)", rec.start_ms));
    }
    parts.push("证据：浏览器下载记录 (只读快照)".to_string());
    SourceInfo {
        kind: "download".to_string(),
        label: headline,
        detail: Some(parts.join("\n")),
    }
}

fn clean_url(url: &str) -> String {
    let u = url.trim();
    if u.is_empty() || u == "about:blank" {
        return String::new();
    }
    // Show host, not a 300-character tracking URL.
    if let Some(rest) = u.split_once("://") {
        let host = rest.1.split('/').next().unwrap_or("");
        return host.to_string();
    }
    if u.chars().count() > 80 {
        let short: String = u.chars().take(80).collect();
        format!("{}…", short)
    } else {
        u.to_string()
    }
}

/// Copy the live DB (and its WAL) to a temp file, then open read-only.
fn snapshot_db(src: &Path) -> Option<PathBuf> {
    let suffix = format!(
        "fl_{}_{}.sqlite",
        process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos()
    );
    let dst = std::env::temp_dir().join(suffix);
    fs::copy(src, &dst).ok()?;
    for ext in ["-wal", "-shm"] {
        let wal = PathBuf::from(format!("{}{}", src.to_string_lossy(), ext));
        if wal.is_file() {
            let target = PathBuf::from(format!("{}{}", dst.to_string_lossy(), ext));
            let _ = fs::copy(&wal, &target);
        }
    }
    Some(dst)
}

fn cleanup_snapshot(path: &Path) {
    let _ = fs::remove_file(path);
    for ext in ["-wal", "-shm"] {
        let _ = fs::remove_file(PathBuf::from(format!("{}{}", path.to_string_lossy(), ext)));
    }
}

fn read_chromium_history(db: &Path, browser: &str, out: &mut Vec<Download>) -> usize {
    let snap = match snapshot_db(db) {
        Some(p) => p,
        None => return 0,
    };
    let conn = match Connection::open_with_flags(&snap, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(c) => c,
        Err(_) => {
            cleanup_snapshot(&snap);
            return 0;
        }
    };

    // Modern Chromium has site_url / tab_url / received_bytes. Older schemas
    // fall back to the minimal three columns.
    let sqls = [
        "SELECT target_path, current_path, site_url, tab_url, received_bytes, start_time FROM downloads",
        "SELECT target_path, current_path, referrer, tab_url, received_bytes, start_time FROM downloads",
        "SELECT target_path, current_path, '', '', 0, start_time FROM downloads",
    ];
    let mut added = 0usize;
    for sql in sqls {
        let mut stmt = match conn.prepare(sql) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                row.get::<_, Option<i64>>(4)?.unwrap_or(0),
                row.get::<_, Option<i64>>(5)?.unwrap_or(0),
            ))
        });
        let mut rows = match rows {
            Ok(r) => r,
            Err(_) => continue,
        };
        for row in rows.by_ref().flatten() {
            let (target, current, site, tab, bytes, start) = row;
            let chosen = if target.is_empty() { &current } else { &target };
            if chosen.is_empty() {
                continue;
            }
            let normalized = chosen.replace('/', "\\");
            let name = Path::new(&normalized)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            out.push(Download {
                path_key: path_key(&normalized),
                name_key: name.to_lowercase(),
                site,
                tab_url: tab,
                browser: browser.to_string(),
                start_ms: chrome_time_to_ms(start),
                bytes,
            });
            added += 1;
        }
        break; // First schema that worked wins.
    }
    drop(conn);
    cleanup_snapshot(&snap);
    added
}

fn read_firefox_places(db: &Path, out: &mut Vec<Download>) -> usize {
    let snap = match snapshot_db(db) {
        Some(p) => p,
        None => return 0,
    };
    let conn = match Connection::open_with_flags(&snap, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(c) => c,
        Err(_) => {
            cleanup_snapshot(&snap);
            return 0;
        }
    };
    let sql = "SELECT a.content, p.url, a.dateAdded FROM moz_annos a \
               JOIN moz_anno_attributes t ON t.id = a.anno_attribute_id \
               JOIN moz_places p ON p.id = a.place_id \
               WHERE t.name = 'downloads/destinationFileURI'";
    let mut added = 0usize;
    if let Ok(mut stmt) = conn.prepare(sql) {
        if let Ok(mut rows) = stmt.query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                row.get::<_, Option<i64>>(2)?.unwrap_or(0),
            ))
        }) {
            for row in rows.by_ref().flatten() {
                let (content, url, date_us) = row;
                let plain = content
                    .strip_prefix("file:///")
                    .unwrap_or(content.as_str())
                    .replace('/', "\\");
                if plain.is_empty() {
                    continue;
                }
                let decoded = percent_decode(&plain);
                let plain = decoded.unwrap_or(plain);
                let name = Path::new(&plain)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                out.push(Download {
                    path_key: path_key(&plain),
                    name_key: name,
                    site: clean_url(&url),
                    tab_url: url,
                    browser: "Firefox".to_string(),
                    // Firefox dateAdded is microseconds since Unix epoch.
                    start_ms: date_us / 1000,
                    bytes: 0,
                });
                added += 1;
            }
        }
    }
    drop(conn);
    cleanup_snapshot(&snap);
    added
}

fn percent_decode(s: &str) -> Option<String> {
    if !s.contains('%') {
        return None;
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(a), Some(b)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(a * 16 + b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    Some(String::from_utf8_lossy(&out).to_string())
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Path-shape classification: the fallback when no download record exists.
pub fn classify_path(path: &str) -> SourceInfo {
    let low = path.to_lowercase().replace('/', "\\");
    let (kind, label, detail) = if low.contains("\\content.outlook\\") {
        (
            "email",
            "Outlook 邮件附件",
            "文件位于 Outlook 的安全附件缓存目录 (Content.Outlook)，通常由「打开附件」或「另存为」产生。",
        )
    } else if low.contains("\\attachments\\") || low.contains("\\attachments_temp\\") {
        ("email", "邮件附件", "路径特征：邮件客户端附件目录。")
    } else if low.contains("\\xwechat_files\\")
        || low.contains("\\wechat files\\")
        || low.contains("\\wechat_files\\")
    {
        (
            "im",
            "微信接收文件",
            "路径特征：微信 FileStorage 接收目录。",
        )
    } else if low.contains("\\wxwork\\") || low.contains("\\企业微信\\") {
        ("im", "企业微信接收文件", "路径特征：企业微信缓存目录。")
    } else if low.contains("\\filerecv\\") || low.contains("\\tencent files\\") {
        ("im", "QQ 接收文件", "路径特征：QQ FileRecv 目录。")
    } else if low.contains("\\feishu\\") || low.contains("\\lark\\") {
        ("im", "飞书接收文件", "路径特征：飞书下载目录。")
    } else if low.contains("\\dingtalk\\") || low.contains("\\钉钉\\") {
        ("im", "钉钉接收文件", "路径特征：钉钉下载目录。")
    } else if low.contains("\\telegram desktop\\") {
        ("im", "Telegram 接收文件", "路径特征：Telegram 下载目录。")
    } else if low.contains("\\downloads\\") || low.contains("\\下载\\") {
        (
            "download",
            "下载目录",
            "文件出现在系统下载目录，但未在浏览器下载记录中找到匹配条目。",
        )
    } else if low.contains("\\desktop\\") || low.contains("\\桌面\\") {
        (
            "local",
            "桌面创建",
            "文件出现在桌面，通常由本机应用直接创建或由用户拖入。",
        )
    } else if low.contains("\\documents\\") || low.contains("\\文档\\") {
        (
            "local",
            "本地生成",
            "文件出现在文档目录，未匹配到外部来源记录。",
        )
    } else {
        (
            "unknown",
            "来源未知",
            "尚无来源证据；后续写入事件可能补充进程归因信息。",
        )
    };
    SourceInfo {
        kind: kind.to_string(),
        label: label.to_string(),
        detail: Some(format!("{}\n证据：路径特征匹配（只读）", detail)),
    }
}

/// Upgrade a weak classification using the process that touched the file.
pub fn refine_with_process(mut info: SourceInfo, process: &str) -> SourceInfo {
    let p = process.to_lowercase();
    if info.kind != "local" && info.kind != "unknown" {
        return info;
    }
    let replacement: Option<(&str, String, &str)> = if p.contains("chrome")
        || p.contains("msedge")
        || p.contains("firefox")
        || p.contains("brave")
        || p.contains("vivaldi")
        || p.contains("opera")
    {
        Some((
            "download",
            "浏览器写入".into(),
            "写句柄归属到浏览器进程，判定为下载产物。",
        ))
    } else if p.contains("outlook") || p.contains("foxmail") || p.contains("thunderbird") {
        Some((
            "email",
            "邮件客户端写入".into(),
            "写句柄归属到邮件客户端进程。",
        ))
    } else if p.contains("wechat")
        || p.contains("weixin")
        || p.contains("qq.exe")
        || p.contains("dingtalk")
        || p.contains("feishu")
        || p.contains("lark")
        || p.contains("telegram")
        || p.contains("slack")
    {
        Some((
            "im",
            "IM 客户端写入".into(),
            "写句柄归属到即时通讯客户端进程。",
        ))
    } else if !p.is_empty() && p != "未知进程" {
        Some((
            "local",
            format!("{} 生成", process),
            "写句柄归属到本机应用进程，判定为本地生成。",
        ))
    } else {
        None
    };

    if let Some((kind, label, why)) = replacement {
        info.kind = kind.to_string();
        info.label = label;
        info.detail = Some(format!("证据：Restart Manager 句柄归因\n{}", why));
    }
    info
}
