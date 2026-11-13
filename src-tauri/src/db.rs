use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use rusqlite::{params, Connection, OptionalExtension};

use crate::attrib::Attribution;
use crate::model::*;
use crate::util::{human_size, now_ms};

#[derive(Debug, Clone)]
pub struct SourceInfo {
    pub kind: String,
    pub label: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FileObservation {
    pub path: String,
    pub name: String,
    pub ext: String,
    pub size: i64,
    pub mtime: i64,
}

impl FileObservation {
    pub fn from_path(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        if !meta.is_file() {
            return None;
        }
        let name = path.file_name()?.to_string_lossy().to_string();
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        Some(Self {
            path: path.to_string_lossy().to_string(),
            name: name.clone(),
            ext: crate::util::extension_of(&name),
            size: meta.len() as i64,
            mtime,
        })
    }
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS files (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    path          TEXT    NOT NULL UNIQUE,
    name          TEXT    NOT NULL,
    ext           TEXT    NOT NULL DEFAULT '',
    size          INTEGER NOT NULL DEFAULT 0,
    mtime         INTEGER NOT NULL DEFAULT 0,
    content_hash  TEXT,
    hash_state    TEXT    NOT NULL DEFAULT 'pending',
    source_kind   TEXT    NOT NULL DEFAULT 'unknown',
    source_label  TEXT    NOT NULL DEFAULT '',
    source_detail TEXT,
    created_at    INTEGER NOT NULL,
    last_seen_at  INTEGER NOT NULL,
    change_count  INTEGER NOT NULL DEFAULT 0,
    liveness      TEXT    NOT NULL DEFAULT 'live'
);
CREATE INDEX IF NOT EXISTS idx_files_hash  ON files(content_hash);
CREATE INDEX IF NOT EXISTS idx_files_name  ON files(name);
CREATE INDEX IF NOT EXISTS idx_files_seen  ON files(last_seen_at);

CREATE TABLE IF NOT EXISTS events (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    file_id       INTEGER NOT NULL,
    kind          TEXT    NOT NULL,
    ts            INTEGER NOT NULL,
    size          INTEGER NOT NULL DEFAULT 0,
    process_name  TEXT,
    process_path  TEXT,
    pid           INTEGER,
    attribution   TEXT    NOT NULL DEFAULT 'unknown',
    note          TEXT
);
CREATE INDEX IF NOT EXISTS idx_events_file ON events(file_id, ts DESC);
CREATE INDEX IF NOT EXISTS idx_events_proc ON events(process_name);
"#;

/// Bare column names on the right-hand side refer to the *pre-update* row,
/// which lets us invalidate the content fingerprint only when size/mtime
/// actually changed (metadata-only touches otherwise cost nothing).
const UPSERT_SQL: &str = r#"
INSERT INTO files
    (path, name, ext, size, mtime, source_kind, source_label, source_detail,
     hash_state, created_at, last_seen_at, change_count, liveness)
VALUES
    (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending', ?9, ?9, 0, 'live')
ON CONFLICT(path) DO UPDATE SET
    last_seen_at  = excluded.last_seen_at,
    liveness      = 'live',
    source_kind   = excluded.source_kind,
    source_label  = excluded.source_label,
    source_detail = excluded.source_detail,
    content_hash  = CASE WHEN size <> excluded.size OR mtime <> excluded.mtime
                         THEN NULL ELSE content_hash END,
    hash_state    = CASE WHEN size <> excluded.size OR mtime <> excluded.mtime
                         THEN 'pending' ELSE hash_state END,
    size          = excluded.size,
    mtime         = excluded.mtime
"#;

const BASELINE_EVENT_SQL: &str = r#"
INSERT INTO events (file_id, kind, ts, size, attribution, note)
SELECT id, 'observed', ?2, ?3, 'baseline',
       '首次扫描登记：只代表我们发现了这个文件，不代表它的创建时间'
  FROM files
 WHERE path = ?1
   AND NOT EXISTS (SELECT 1 FROM events e WHERE e.file_id = files.id)
"#;

const LIST_SQL: &str = r#"
SELECT f.id, f.path, f.name, f.ext, f.size, f.mtime,
       f.source_kind, f.source_label, f.source_detail,
       f.change_count, f.content_hash, f.hash_state, f.liveness,
       (SELECT COUNT(*) FROM files d
         WHERE d.content_hash IS NOT NULL
           AND d.content_hash = f.content_hash
           AND d.id <> f.id
           AND d.liveness = 'live') AS dup_count,
       COALESCE((SELECT MAX(e.ts) FROM events e WHERE e.file_id = f.id), f.created_at) AS last_ts,
       f.created_at
  FROM files f
 WHERE (?1 = '' OR lower(f.name) LIKE ?2 OR lower(f.path) LIKE ?2)
 ORDER BY last_ts DESC
 LIMIT ?3 OFFSET ?4
"#;

pub struct Store {
    conn: Mutex<Connection>,
    pub db_path: PathBuf,
}

impl Store {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(path)?;
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        let _ = conn.pragma_update(None, "synchronous", "NORMAL");
        let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
            db_path: path.to_path_buf(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn db_size(&self) -> i64 {
        std::fs::metadata(&self.db_path)
            .map(|m| m.len() as i64)
            .unwrap_or(0)
    }

    /// Fast path for the initial directory walk: one transaction per chunk.
    pub fn register_batch(
        &self,
        items: &[(FileObservation, SourceInfo)],
    ) -> rusqlite::Result<usize> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let now = now_ms();
        {
            let mut upsert = tx.prepare_cached(UPSERT_SQL)?;
            let mut baseline = tx.prepare_cached(BASELINE_EVENT_SQL)?;
            for (obs, src) in items {
                upsert.execute(params![
                    obs.path, obs.name, obs.ext, obs.size, obs.mtime, src.kind, src.label,
                    src.detail, now
                ])?;
                baseline.execute(params![obs.path, now, obs.size])?;
            }
        }
        tx.commit()?;
        Ok(items.len())
    }

    pub fn upsert_file(&self, obs: &FileObservation, src: &SourceInfo) -> rusqlite::Result<i64> {
        let conn = self.lock();
        let now = now_ms();
        conn.execute(
            UPSERT_SQL,
            params![
                obs.path, obs.name, obs.ext, obs.size, obs.mtime, src.kind, src.label, src.detail,
                now
            ],
        )?;
        conn.query_row(
            "SELECT id FROM files WHERE path = ?1",
            params![obs.path],
            |r| r.get(0),
        )
    }

    /// Returns `false` when the event was suppressed by the throttle.
    pub fn record_event(
        &self,
        file_id: i64,
        kind: &str,
        ts: i64,
        size: i64,
        attr: Option<&Attribution>,
        note: Option<&str>,
    ) -> rusqlite::Result<bool> {
        let conn = self.lock();
        let last: Option<(String, i64, i64)> = conn
            .query_row(
                "SELECT kind, size, ts FROM events WHERE file_id = ?1 ORDER BY id DESC LIMIT 1",
                params![file_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if let Some((last_kind, last_size, last_ts)) = last {
            if last_kind == kind && last_size == size && ts - last_ts < 1_500 {
                return Ok(false);
            }
        }

        let (pname, ppath, pid, method) = match attr {
            Some(a) => (
                a.process_name.clone(),
                a.process_path.clone(),
                a.pid.map(|p| p as i64),
                a.method.clone(),
            ),
            None => (None, None, None, "unknown".to_string()),
        };
        conn.execute(
            "INSERT INTO events (file_id, kind, ts, size, process_name, process_path, pid, attribution, note)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![file_id, kind, ts, size, pname, ppath, pid, method, note],
        )?;
        conn.execute(
            "UPDATE files SET change_count = change_count + 1 WHERE id = ?1",
            params![file_id],
        )?;
        Ok(true)
    }

    pub fn mark_gone(&self, path: &str) -> rusqlite::Result<()> {
        self.lock().execute(
            "UPDATE files SET liveness = 'gone' WHERE path = ?1",
            params![path],
        )?;
        Ok(())
    }

    pub fn id_for_path(&self, path: &str) -> Option<i64> {
        let conn = self.lock();
        conn.query_row("SELECT id FROM files WHERE path = ?1", params![path], |r| {
            r.get(0)
        })
        .ok()
    }

    pub fn counts(&self) -> (i64, i64, i64, i64) {
        let conn = self.lock();
        let read = || -> rusqlite::Result<(i64, i64, i64, i64)> {
            conn.query_row(
                "SELECT
                   (SELECT COUNT(*) FROM files),
                   (SELECT COUNT(*) FROM events),
                   (SELECT COUNT(*) FROM files WHERE hash_state = 'done'),
                   (SELECT COUNT(*) FROM files WHERE hash_state = 'pending' AND liveness = 'live')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
        };
        read().unwrap_or((0, 0, 0, 0))
    }

    pub fn pending_hash_batch(&self, limit: i64) -> Vec<(i64, String, i64)> {
        let conn = self.lock();
        let mut stmt = match conn.prepare(
            "SELECT id, path, size FROM files
              WHERE hash_state = 'pending' AND liveness = 'live'
              ORDER BY size ASC LIMIT ?1",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(params![limit], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)));
        match rows {
            Ok(rows) => rows.flatten().collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn set_hash(&self, id: i64, hash: &str) {
        let _ = self.lock().execute(
            "UPDATE files SET content_hash = ?1, hash_state = 'done' WHERE id = ?2",
            params![hash, id],
        );
    }

    pub fn set_hash_state(&self, id: i64, state: &str) {
        let _ = self.lock().execute(
            "UPDATE files SET hash_state = ?1 WHERE id = ?2",
            params![state, id],
        );
    }

    pub fn list_files(&self, query: &str, limit: i64, offset: i64) -> Vec<FileRow> {
        let conn = self.lock();
        let mut stmt = match conn.prepare(LIST_SQL) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let pattern = format!("%{}%", query.to_lowercase());
        let rows = stmt.query_map(params![query, pattern, limit, offset], row_to_file);
        match rows {
            Ok(rows) => rows.flatten().collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn events_for_file(&self, id: i64) -> Vec<EventRow> {
        let conn = self.lock();
        let mut stmt = match conn.prepare(
            "SELECT id, kind, ts, size, process_name, process_path, pid, attribution, note
               FROM events WHERE file_id = ?1 ORDER BY ts DESC, id DESC LIMIT 500",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(params![id], |r| {
            Ok(EventRow {
                id: r.get(0)?,
                kind: r.get(1)?,
                ts: r.get(2)?,
                size: r.get(3)?,
                process_name: r.get(4)?,
                process_path: r.get(5)?,
                pid: r.get(6)?,
                attribution: r.get(7)?,
                note: r.get(8)?,
            })
        });
        match rows {
            Ok(rows) => rows.flatten().collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn copies(&self, hash: &str, exclude_id: i64) -> Vec<CopyRow> {
        let conn = self.lock();
        let mut stmt = match conn.prepare(
            "SELECT id, path, size, mtime FROM files
              WHERE content_hash = ?1 AND id <> ?2 AND liveness = 'live'
              ORDER BY path LIMIT 100",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(params![hash, exclude_id], |r| {
            Ok(CopyRow {
                id: r.get(0)?,
                path: r.get(1)?,
                size: r.get(2)?,
                mtime: r.get(3)?,
            })
        });
        match rows {
            Ok(rows) => rows.flatten().collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn file_row(&self, id: i64) -> Option<FileRow> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT f.id, f.path, f.name, f.ext, f.size, f.mtime,
                        f.source_kind, f.source_label, f.source_detail,
                        f.change_count, f.content_hash, f.hash_state, f.liveness,
                        (SELECT COUNT(*) FROM files d
                          WHERE d.content_hash IS NOT NULL
                            AND d.content_hash = f.content_hash
                            AND d.id <> f.id AND d.liveness = 'live'),
                        COALESCE((SELECT MAX(e.ts) FROM events e WHERE e.file_id = f.id), f.created_at),
                        f.created_at
                   FROM files f WHERE f.id = ?1",
            )
            .ok()?;
        stmt.query_row(params![id], row_to_file).ok()
    }

    pub fn file_detail(&self, id: i64) -> Option<FileDetail> {
        let file = self.file_row(id)?;
        let versions = self.events_for_file(id);
        let copies = match &file.content_hash {
            Some(h) => self.copies(h, id),
            None => Vec::new(),
        };
        let answers = build_answers(&file, &versions, &copies);
        let graph = build_graph(&file, &versions, &copies);
        Some(FileDetail {
            file,
            versions,
            copies,
            graph,
            answers,
        })
    }
}

fn row_to_file(r: &rusqlite::Row<'_>) -> rusqlite::Result<FileRow> {
    Ok(FileRow {
        id: r.get(0)?,
        path: r.get(1)?,
        name: r.get(2)?,
        ext: r.get(3)?,
        size: r.get(4)?,
        mtime: r.get(5)?,
        source_kind: r.get(6)?,
        source_label: r.get(7)?,
        source_detail: r.get(8)?,
        change_count: r.get(9)?,
        content_hash: r.get(10)?,
        hash_state: r.get(11)?,
        liveness: r.get(12)?,
        duplicate_count: r.get(13)?,
        last_event_ts: r.get(14)?,
        created_at: r.get(15)?,
    })
}

fn build_answers(file: &FileRow, versions: &[EventRow], copies: &[CopyRow]) -> Answers {
    let origin_headline = match file.source_kind.as_str() {
        "download" => "来自外部下载",
        "email" => "来自邮件",
        "im" => "来自聊天工具",
        "local" => "本机生成",
        _ => "来源未知",
    };
    let origin_conf = match file.source_kind.as_str() {
        "download" | "email" | "im" => "high",
        "local" => "medium",
        _ => "low",
    };
    let origin_detail = file.source_detail.clone().unwrap_or_else(|| {
        "没有找到任何来源记录。可能是很早以前就存在的文件，或由未接入适配器的数据源产生。".into()
    });

    let mut proc_counts: Vec<(String, usize)> = Vec::new();
    for ev in versions {
        if let Some(p) = &ev.process_name {
            if let Some(entry) = proc_counts.iter_mut().find(|(n, _)| n == p) {
                entry.1 += 1;
            } else {
                proc_counts.push((p.clone(), 1));
            }
        }
    }
    proc_counts.sort_by(|a, b| b.1.cmp(&a.1));
    let (mod_headline, mod_detail, mod_conf) = if proc_counts.is_empty() {
        (
            "尚未捕获到写入进程".to_string(),
            "文件出现或变化时没有拿到归属进程。常见原因：写入太快、进程已关闭句柄、或文件由扫描登记。\
V2 会接入 ETW 内核事件流以做到无遗漏归因。".to_string(),
            "low".to_string(),
        )
    } else {
        let names = proc_counts
            .iter()
            .take(5)
            .map(|(n, c)| format!("{}（{} 次）", n, c))
            .collect::<Vec<_>>()
            .join("、");
        (
            format!("{} 个进程碰过它", proc_counts.len()),
            format!(
                "按事件次数排序：{}。证据来自 Windows Restart Manager 的句柄归属（无需管理员权限）。\n时间线共 {} 条事件。",
                names,
                versions.len()
            ),
            "medium".to_string(),
        )
    };

    let fingerprint: String = file
        .content_hash
        .as_deref()
        .map(|h| h.chars().take(16).collect())
        .unwrap_or_default();
    let (safety_headline, safety_detail, safety_conf) = match &file.content_hash {
        None => (
            "内容指纹尚未生成".to_string(),
            format!(
                "后台哈希队列状态：{}。完成后才能判断是否存在相同内容的其他副本。",
                file.hash_state
            ),
            "low".to_string(),
        ),
        Some(_) if copies.is_empty() => {
            let extra = if file.source_kind == "download" {
                "\n不过它被判定为下载产物，若原始 URL 仍有效，可以重新获取。"
            } else {
                ""
            };
            (
                "唯一副本，删除会丢内容".to_string(),
                format!(
                    "在已监控目录中，BLAKE3 指纹 {}… 只出现一次（{}）。删除后本机不再有相同内容。{}",
                    fingerprint,
                    human_size(file.size),
                    extra
                ),
                "high".to_string(),
            )
        }
        Some(_) => {
            let list = copies
                .iter()
                .take(4)
                .map(|c| format!("  · {}", c.path))
                .collect::<Vec<_>>()
                .join("\n");
            let more = if copies.len() > 4 {
                format!("\n  · …另有 {} 处", copies.len() - 4)
            } else {
                String::new()
            };
            (
                format!("还有 {} 个副本，删除不会丢内容", copies.len()),
                format!(
                    "BLAKE3 指纹 {}… 在以下位置重复出现：\n{}{}\n内容完全一致，删除本文件不会丢失数据。",
                    fingerprint, list, more
                ),
                "high".to_string(),
            )
        }
    };

    Answers {
        origin: Answer {
            headline: origin_headline.to_string(),
            detail: origin_detail,
            confidence: origin_conf.to_string(),
        },
        modifiers: Answer {
            headline: mod_headline,
            detail: mod_detail,
            confidence: mod_conf,
        },
        safety: Answer {
            headline: safety_headline,
            detail: safety_detail,
            confidence: safety_conf,
        },
    }
}

fn build_graph(file: &FileRow, versions: &[EventRow], copies: &[CopyRow]) -> Graph {
    let mut graph = Graph::default();
    let file_id = format!("file:{}", file.id);
    let source_id = format!("source:{}", file.id);

    graph.nodes.push(GraphNode {
        id: source_id.clone(),
        label: file.source_label.clone(),
        kind: "source".into(),
        sub: Some(file.source_kind.clone()),
        weight: 1.2,
    });
    graph.nodes.push(GraphNode {
        id: file_id.clone(),
        label: file.name.clone(),
        kind: "file".into(),
        sub: Some(human_size(file.size)),
        weight: 2.2,
    });
    graph.edges.push(GraphEdge {
        from: source_id,
        to: file_id.clone(),
        label: "来源于".into(),
        kind: "origin".into(),
    });

    let chronological: Vec<&EventRow> = versions.iter().rev().take(12).collect();
    let mut prev: Option<String> = None;
    let mut processes: Vec<String> = Vec::new();
    for (i, ev) in chronological.iter().enumerate() {
        let node = format!("ver:{}", ev.id);
        graph.nodes.push(GraphNode {
            id: node.clone(),
            label: format!("v{}", i + 1),
            kind: "version".into(),
            sub: Some(event_label(&ev.kind)),
            weight: 1.0,
        });
        if let Some(p) = &prev {
            graph.edges.push(GraphEdge {
                from: p.clone(),
                to: node.clone(),
                label: "演进".into(),
                kind: "evolve".into(),
            });
        }
        if let Some(p) = &ev.process_name {
            let pid = format!("proc:{}", p.to_lowercase());
            if !processes.contains(&pid) {
                processes.push(pid.clone());
                graph.nodes.push(GraphNode {
                    id: pid.clone(),
                    label: p.clone(),
                    kind: "process".into(),
                    sub: Some("持有写句柄".into()),
                    weight: 1.1,
                });
            }
            graph.edges.push(GraphEdge {
                from: pid,
                to: node.clone(),
                label: "写入".into(),
                kind: "touch".into(),
            });
        }
        prev = Some(node);
    }
    if let Some(p) = prev {
        graph.edges.push(GraphEdge {
            from: p,
            to: file_id.clone(),
            label: "当前版本".into(),
            kind: "evolve".into(),
        });
    }

    for c in copies.iter().take(10) {
        let node = format!("copy:{}", c.id);
        graph.nodes.push(GraphNode {
            id: node.clone(),
            label: Path::new(&c.path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| c.path.clone()),
            kind: "copy".into(),
            sub: Some(c.path.clone()),
            weight: 0.9,
        });
        graph.edges.push(GraphEdge {
            from: file_id.clone(),
            to: node,
            label: "内容相同".into(),
            kind: "copy".into(),
        });
    }
    graph
}

fn event_label(kind: &str) -> String {
    match kind {
        "created" => "创建".into(),
        "modified" => "修改".into(),
        "renamed" => "重命名".into(),
        "removed" => "删除".into(),
        "observed" => "首次登记".into(),
        other => other.to_string(),
    }
}
