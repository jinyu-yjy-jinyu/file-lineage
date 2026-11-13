//! Source adapters for Q1 ("这个文件是哪来的").
//!
//! Adapters are deliberately read-only and side-effect free:
//!   * browser download records — Chrome / Edge / Brave / Vivaldi / Chromium
//!     `History` SQLite, Firefox `places.sqlite` (copied to a temp file first,
//!     because browsers keep the live DB locked);
//!   * path-shape heuristics for mail attachments and IM receive folders.
//!
//! Every result carries an **evidence id** and a **strength**, which is what
//! the answer engine and the combined verdict derive their confidence from:
//!
//! | strength | meaning | examples |
//! | --- | --- | --- |
//! | 3 | objective / authoritative | `zone.ads`, `downloader.task`, exact browser path hit |
//! | 2 | strong indirect | `browser.name_size`, `im.layout`, `rm.handle` (writer only) |
//! | 1 | heuristic | `path.shape`, `mail.ext` |
//! | 0 | none | `none` |
//!
//! A path heuristic must never be reported as "high confidence" — that was the
//! core defect this module fixes.

use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use rusqlite::{Connection, OpenFlags};

use crate::downloaders::{self, Downloaders};
use crate::im_layout;
use crate::util::{format_ms, path_key};
use crate::zoneid;
use crate::SourceInfo;

/// Chrome/Edge/Firefox timestamps: microseconds since 1601-01-01.
const CHROME_EPOCH_OFFSET_MS: i64 = 11_644_473_600_000;

pub const EVIDENCE_ZONE: &str = "zone.ads";
pub const EVIDENCE_DOWNLOADER: &str = "downloader.task";
pub const EVIDENCE_BROWSER_EXACT: &str = "browser.exact";
pub const EVIDENCE_BROWSER_NAME_SIZE: &str = "browser.name_size";
pub const EVIDENCE_IM_LAYOUT: &str = "im.layout";
pub const EVIDENCE_MAIL_EXT: &str = "mail.ext";
pub const EVIDENCE_PATH: &str = "path.shape";
pub const EVIDENCE_HANDLE: &str = "rm.handle";
pub const EVIDENCE_NONE: &str = "none";

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
    /// Third-party downloader task logs (Ghost Downloader, …).
    pub downloaders: Downloaders,
    adapters: Vec<String>,
}

impl Sources {
    pub fn load() -> Self {
        let mut this = Sources::default();
        this.downloaders = Downloaders::load();
        if let Some(label) = &this.downloaders.adapter_label {
            this.adapters.push(label.clone());
        }
        this.load_chromium();
        this.load_firefox();
        this
    }

    pub fn adapter_names(&self) -> &[String] {
        &self.adapters
    }

    pub fn record_count(&self) -> usize {
        self.downloads.len() + self.downloaders.records().len()
    }

    fn load_chromium(&mut self) {
        for (browser, root) in chromium_roots() {
            if !root.is_dir() {
                continue;
            }
            let mut found = 0usize;
            let mut incomplete = false;
            let entries = match fs::read_dir(&root) {
                Ok(e) => e,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let history = entry.path().join("History");
                if !history.is_file() {
                    continue;
                }
                let (n, ok) = read_chromium_history(&history, browser, &mut self.downloads);
                found += n;
                incomplete |= !ok;
            }
            if incomplete {
                self.adapters.push(format!(
                    "{}（快照不完整，已跳过 {} 条可能过期的记录）",
                    browser, found
                ));
            } else if found > 0 {
                self.adapters
                    .push(format!("{} ({} 条下载记录)", browser, found));
            }
        }
    }

    fn load_firefox(&mut self) {
        let Some(root) = firefox_root() else {
            return;
        };
        let entries = match fs::read_dir(root) {
            Ok(e) => e,
            Err(_) => return,
        };
        let mut found = 0usize;
        let mut incomplete = false;
        for entry in entries.flatten() {
            let places = entry.path().join("places.sqlite");
            if places.is_file() {
                let (n, ok) = read_firefox_places(&places, &mut self.downloads);
                found += n;
                incomplete |= !ok;
            }
        }
        if incomplete {
            self.adapters.push(format!(
                "Firefox（快照不完整，已跳过 {} 条可能过期的记录）",
                found
            ));
        } else if found > 0 {
            self.adapters
                .push(format!("Firefox ({} 条下载记录)", found));
        }
    }

    /// Exact path match first, then a conservative name+size match inside a
    /// Downloads folder (covers "downloaded then moved to Desktop").
    pub fn lookup(&self, path: &str, name: &str, size: i64) -> Option<SourceInfo> {
        let key = path_key(path);
        if let Some(rec) = self.downloads.iter().find(|d| d.path_key == key) {
            return Some(to_source_info(rec, EVIDENCE_BROWSER_EXACT, 3));
        }
        let lname = name.to_lowercase();
        if let Some(rec) = self
            .downloads
            .iter()
            .find(|d| d.name_key == lname && (d.bytes <= 0 || size <= 0 || d.bytes == size))
        {
            return Some(to_source_info(rec, EVIDENCE_BROWSER_NAME_SIZE, 2));
        }
        None
    }
}

/// Chromium-family profile roots for the current OS. The env-var lookup below
/// is inside `#[cfg(windows)]`, so no non-Windows target ever references a
/// Windows-only variable.
fn chromium_roots() -> Vec<(&'static str, PathBuf)> {
    let mut out: Vec<(&'static str, PathBuf)> = Vec::new();

    #[cfg(windows)]
    {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            let base = PathBuf::from(local);
            out.push((
                "Chrome",
                base.join("Google").join("Chrome").join("User Data"),
            ));
            out.push((
                "Edge",
                base.join("Microsoft").join("Edge").join("User Data"),
            ));
            out.push((
                "Brave",
                base.join("BraveSoftware")
                    .join("Brave-Browser")
                    .join("User Data"),
            ));
            out.push(("Vivaldi", base.join("Vivaldi").join("User Data")));
            out.push(("Chromium", base.join("Chromium").join("User Data")));
        }
    }

    #[cfg(target_os = "macos")]
    {
        // ~/Library/Application Support
        if let Some(base) = dirs::data_dir() {
            out.push(("Chrome", base.join("Google").join("Chrome")));
            out.push(("Edge", base.join("Microsoft Edge")));
            out.push(("Brave", base.join("BraveSoftware").join("Brave-Browser")));
            out.push(("Vivaldi", base.join("Vivaldi")));
            out.push(("Chromium", base.join("Chromium")));
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(base) = dirs::config_dir() {
            out.push(("Chrome", base.join("google-chrome")));
            out.push(("Edge", base.join("microsoft-edge")));
            out.push(("Brave", base.join("BraveSoftware").join("Brave-Browser")));
            out.push(("Vivaldi", base.join("vivaldi")));
            out.push(("Chromium", base.join("chromium")));
        }
    }

    out
}

fn firefox_root() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        // %APPDATA%\Mozilla\Firefox\Profiles
        std::env::var("APPDATA").ok().map(|appdata| {
            PathBuf::from(appdata)
                .join("Mozilla")
                .join("Firefox")
                .join("Profiles")
        })
    }
    #[cfg(target_os = "macos")]
    {
        dirs::data_dir().map(|base| base.join("Firefox").join("Profiles"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        dirs::home_dir().map(|home| home.join(".mozilla").join("firefox"))
    }
}

fn to_source_info(rec: &Download, evidence: &str, strength: i64) -> SourceInfo {
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
        // Explicitly UTC: the UI renders local time, and an unlabelled number
        // would read as a different moment.
        parts.push(format!("下载时间：{}", format_ms(rec.start_ms)));
    }
    if evidence == EVIDENCE_BROWSER_NAME_SIZE {
        parts.push("匹配方式：文件名 + 大小（非精确路径），来源主机可能不准确".to_string());
    }
    parts.push("证据：浏览器下载记录 (只读快照)".to_string());
    SourceInfo {
        kind: "download".to_string(),
        label: headline,
        detail: Some(parts.join("\n")),
        evidence: evidence.to_string(),
        strength,
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
///
/// A half-copied snapshot (main DB copied, `-wal` copy failed) would silently
/// read *stale* rows, so that case is an error and the caller skips the library
/// instead of reporting a wrong answer.
fn snapshot_db(src: &Path) -> Result<PathBuf, String> {
    let suffix = format!(
        "fl_{}_{}.sqlite",
        process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let dst = std::env::temp_dir().join(suffix);
    fs::copy(src, &dst).map_err(|e| e.to_string())?;
    for ext in ["-wal", "-shm"] {
        let wal = PathBuf::from(format!("{}{}", src.to_string_lossy(), ext));
        if wal.is_file() {
            let target = PathBuf::from(format!("{}{}", dst.to_string_lossy(), ext));
            if let Err(error) = fs::copy(&wal, &target) {
                cleanup_snapshot(&dst);
                return Err(format!("{} 复制失败：{}", ext, error));
            }
        }
    }
    Ok(dst)
}

fn cleanup_snapshot(path: &Path) {
    let _ = fs::remove_file(path);
    for ext in ["-wal", "-shm"] {
        let _ = fs::remove_file(PathBuf::from(format!("{}{}", path.to_string_lossy(), ext)));
    }
}

/// Returns `(records, snapshot_complete)`.
fn read_chromium_history(db: &Path, browser: &str, out: &mut Vec<Download>) -> (usize, bool) {
    let snap = match snapshot_db(db) {
        Ok(p) => p,
        Err(_) => return (0, false),
    };
    let conn = match Connection::open_with_flags(&snap, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(c) => c,
        Err(_) => {
            cleanup_snapshot(&snap);
            return (0, true);
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
    (added, true)
}

/// Returns `(records, snapshot_complete)`.
fn read_firefox_places(db: &Path, out: &mut Vec<Download>) -> (usize, bool) {
    let snap = match snapshot_db(db) {
        Ok(p) => p,
        Err(_) => return (0, false),
    };
    let conn = match Connection::open_with_flags(&snap, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(c) => c,
        Err(_) => {
            cleanup_snapshot(&snap);
            return (0, true);
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
    (added, true)
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

/// Full source resolution, strongest evidence first.
///
/// Order matters and is the whole point of the source work:
///   1. `Zone.Identifier` — written by the downloader itself, browser
///      independent, survives cleared history, names the real origin host (3);
///   2. **third-party downloader task logs** — exact URL, tool name and
///      timestamp, and the only evidence that works for files downloaded
///      before this app was installed (3);
///   3. browser download databases — exact path (3) or name+size (2);
///   4. IM receive-tree layout — names the account and the receive month (2);
///   5. mail-ish file extensions (1);
///   6. plain path shape (1), or nothing at all (0).
pub fn resolve(sources: &Sources, path: &str, name: &str, size: i64) -> SourceInfo {
    if let Some(zone) = zoneid::read(path) {
        let origin = zone.origin();
        let headline = if origin.is_empty() {
            format!("Internet 下载 · {}", zone.zone_label())
        } else {
            format!("Internet 下载 · {}", origin)
        };
        let mut parts = vec![format!("来源区域：{}", zone.zone_label())];
        if !zone.host.is_empty() {
            parts.push(format!("下载地址：{}", zone.host));
        }
        if !zone.referrer.is_empty() {
            parts.push(format!("来源页面：{}", zone.referrer));
        }
        parts.push("证据：文件 Zone.Identifier 备用数据流（写入方标记，与浏览器无关）".to_string());
        return SourceInfo {
            kind: "download".to_string(),
            label: headline,
            detail: Some(parts.join("\n")),
            evidence: EVIDENCE_ZONE.to_string(),
            strength: 3,
        };
    }

    // Third-party downloaders (Ghost Downloader / Motrix / IDM / …) do not
    // write Zone.Identifier and never appear in browser history. Their own
    // task log is the only place that says "who fetched this, from where,
    // and when". Insert it before browser records so a precise task match wins.
    if let Some(task) = sources.downloaders.lookup(path, name, size) {
        let host = clean_url(&task.url);
        let headline = if host.is_empty() {
            format!("{} 下载", task.vendor)
        } else {
            format!("{} 下载 · {}", task.vendor, host)
        };
        let mut parts = vec![format!("产生方式：由 {} 从网络下载并写入", task.vendor)];
        if !task.url.is_empty() {
            parts.push(format!(
                "下载地址：{}",
                downloaders::truncate(&task.url, 240)
            ));
        }
        if !task.referer.is_empty() {
            parts.push(format!("来源页面：{}", task.referer));
        }
        if task.timestamp() > 0 {
            parts.push(format!("下载完成：{}", format_ms(task.timestamp())));
        }
        if !task.task_id.is_empty() {
            parts.push(format!("任务 ID：{}", task.task_id));
        }
        parts.push(format!(
            "证据：{} 任务记录 tasks.jsonl（只读）",
            task.vendor
        ));
        return SourceInfo {
            kind: "download".to_string(),
            label: headline,
            detail: Some(parts.join("\n")),
            evidence: EVIDENCE_DOWNLOADER.to_string(),
            strength: 3,
        };
    }

    if let Some(found) = sources.lookup(path, name, size) {
        return found;
    }

    if let Some(layout) = im_layout::parse(path) {
        return SourceInfo {
            kind: "im".to_string(),
            label: layout.describe(),
            detail: Some(format!(
                "{}\n证据：IM 接收目录布局解析（账号目录 + 接收月份分桶）",
                layout.describe()
            )),
            evidence: EVIDENCE_IM_LAYOUT.to_string(),
            strength: 2,
        };
    }

    if let Some(hint) = im_layout::mail_extension_hint(name) {
        return SourceInfo {
            kind: "email".to_string(),
            label: hint.to_string(),
            detail: Some(format!("{}\n证据：文件扩展名", hint)),
            evidence: EVIDENCE_MAIL_EXT.to_string(),
            strength: 1,
        };
    }

    classify_path(path)
}

/// Path-shape classification: the fallback when no download record exists.
///
/// This is a *guess*. It is deliberately capped at strength 1, and the
/// `download` case is labelled "来源未确认" so the answer engine cannot turn it
/// into a high-confidence claim.
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
            "下载目录（来源未确认）",
            "文件位于下载目录，但没有匹配到 Zone.Identifier、下载器任务记录或浏览器下载记录。\
常见原因：安装本应用之前就已下载、由未接入的下载器产生、或下载器的任务历史已被清理。\
今后新下载的文件会在产生时记录进程与来源。",
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
    let (evidence, strength) = if kind == "unknown" {
        (EVIDENCE_NONE, 0)
    } else {
        (EVIDENCE_PATH, 1)
    };
    SourceInfo {
        kind: kind.to_string(),
        label: label.to_string(),
        detail: Some(format!("{}\n证据：路径特征匹配（只读，启发式）", detail)),
        evidence: evidence.to_string(),
        strength,
    }
}

/// Upgrade a *weak* classification using the process that touched the file.
///
/// A write handle proves **who wrote it**, never **where it came from**. The
/// result is therefore capped at strength 2 (`rm.handle`), and the wording says
/// "写入者已确定，来源仍未确认". Only a strength ≤ 1 classification may be
/// upgraded — a `Zone.Identifier`/downloader/browser fact is never overwritten
/// by a handle observation.
pub fn refine_with_process(mut info: SourceInfo, process: &str) -> SourceInfo {
    if info.strength >= 2 {
        return info;
    }
    let p = process.to_lowercase();
    if p.is_empty() || p == "未知进程" {
        return info;
    }

    let (kind, label): (&str, String) = if p.contains("chrome")
        || p.contains("msedge")
        || p.contains("firefox")
        || p.contains("brave")
        || p.contains("vivaldi")
        || p.contains("opera")
    {
        (
            "download",
            format!("很可能由 {} 写入（来源未确认）", process),
        )
    } else if p.contains("outlook") || p.contains("foxmail") || p.contains("thunderbird") {
        ("email", format!("很可能由 {} 写入（来源未确认）", process))
    } else if p.contains("wechat")
        || p.contains("weixin")
        || p.contains("qq.exe")
        || p.contains("dingtalk")
        || p.contains("feishu")
        || p.contains("lark")
        || p.contains("telegram")
        || p.contains("slack")
    {
        ("im", format!("很可能由 {} 写入（来源未确认）", process))
    } else {
        ("local", format!("{} 写入", process))
    };

    info.kind = kind.to_string();
    info.label = label;
    info.evidence = EVIDENCE_HANDLE.to_string();
    info.strength = 2;
    info.detail = Some(format!(
        "写入者已确定：{}（Restart Manager 写句柄归因）。\n\
         这只能证明「谁写的」，不能证明「从哪里来」——文件名来源、下载 URL 与来源页面仍然未知。",
        process
    ));
    info
}
