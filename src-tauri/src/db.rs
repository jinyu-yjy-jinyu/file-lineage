use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension};

use crate::attrib::Attribution;
use crate::fingerprint::ChunkDigest;
use crate::model::*;
use crate::util::{human_size, month_bounds_ms, now_ms};
use crate::verdict;

/// Bumped whenever a table/column layout changes.
pub const SCHEMA_VERSION: i64 = 3;
/// Bumped only when the digest algorithm or its inputs change.
pub const HASH_ALGO_VERSION: &str = "2";

#[derive(Debug, Clone)]
pub struct SourceInfo {
    pub kind: String,
    pub label: String,
    pub detail: Option<String>,
    /// Evidence channel id (`zone.ads`, `downloader.task`, `browser.exact`,
    /// `browser.name_size`, `im.layout`, `mail.ext`, `path.shape`, `rm.handle`,
    /// `none`). Stored so confidence can be derived instead of guessed.
    pub evidence: String,
    /// 3 = objective, 2 = strong indirect, 1 = heuristic, 0 = none.
    pub strength: i64,
}

impl SourceInfo {
    /// Fallback used when a source adapter produced nothing at all.
    pub fn unknown() -> Self {
        Self {
            kind: "unknown".to_string(),
            label: "来源未知".to_string(),
            detail: None,
            evidence: "none".to_string(),
            strength: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FileObservation {
    pub path: String,
    pub name: String,
    pub ext: String,
    pub size: i64,
    pub mtime: i64,
    /// NTFS file index + volume serial, when the filesystem exposes them.
    pub volume: Option<i64>,
    pub file_index: Option<i64>,
}

impl FileObservation {
    pub fn from_path(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        if !meta.is_file() {
            return None;
        }
        let name = path.file_name()?.to_string_lossy().to_string();
        let mtime = meta.modified().ok().map(crate::util::millis).unwrap_or(0);
        let identity = crate::identity::file_identity(path);
        Some(Self {
            path: path.to_string_lossy().to_string(),
            name: name.clone(),
            ext: crate::util::extension_of(&name),
            size: meta.len() as i64,
            mtime,
            volume: identity.map(|i| i.volume as i64),
            file_index: identity.map(|i| i.index as i64),
        })
    }
}

/// Phase 1 — tables only.
///
/// This must NEVER reference a column that `migrate` may still have to add.
/// On a V1 database `CREATE TABLE IF NOT EXISTS` is a no-op, so an index over
/// a V2 column here fails the whole `execute_batch` and, because the caller
/// used `expect`, kills the GUI process with no visible error. V2.0.0 shipped
/// exactly that bug: `idx_files_identity` referenced `volume_serial` before
/// the column existed. Indexes therefore live in [`SCHEMA_INDEXES`].
const SCHEMA_TABLES: &str = r#"
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS files (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    path          TEXT    NOT NULL UNIQUE,
    name          TEXT    NOT NULL,
    ext           TEXT    NOT NULL DEFAULT '',
    size          INTEGER NOT NULL DEFAULT 0,
    mtime         INTEGER NOT NULL DEFAULT 0,
    content_hash  TEXT,
    hash_state    TEXT    NOT NULL DEFAULT 'settling',
    hash_reason   TEXT,
    source_kind   TEXT    NOT NULL DEFAULT 'unknown',
    source_label  TEXT    NOT NULL DEFAULT '',
    source_detail TEXT,
    source_evidence TEXT  NOT NULL DEFAULT '',
    source_strength INTEGER NOT NULL DEFAULT 0,
    created_at    INTEGER NOT NULL,
    last_seen_at  INTEGER NOT NULL,
    change_count  INTEGER NOT NULL DEFAULT 0,
    liveness      TEXT    NOT NULL DEFAULT 'live',
    volume_serial INTEGER,
    file_index    INTEGER,
    chunk_count   INTEGER NOT NULL DEFAULT 0,
    opened_ts     INTEGER NOT NULL DEFAULT 0
);

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

CREATE TABLE IF NOT EXISTS chunks (
    file_id INTEGER NOT NULL,
    idx     INTEGER NOT NULL,
    hash    TEXT    NOT NULL,
    PRIMARY KEY (file_id, idx)
);
"#;

/// Phase 3 — indexes. Executed only after `migrate` has guaranteed that every
/// referenced column exists on both fresh and upgraded databases.
const SCHEMA_INDEXES: &str = r#"
CREATE INDEX IF NOT EXISTS idx_files_hash ON files(content_hash);
CREATE INDEX IF NOT EXISTS idx_files_name ON files(name);
CREATE INDEX IF NOT EXISTS idx_files_seen ON files(last_seen_at);
CREATE INDEX IF NOT EXISTS idx_files_identity ON files(volume_serial, file_index);
CREATE INDEX IF NOT EXISTS idx_events_file ON events(file_id, ts DESC);
CREATE INDEX IF NOT EXISTS idx_events_proc ON events(process_name);
CREATE INDEX IF NOT EXISTS idx_events_ts   ON events(ts);
CREATE INDEX IF NOT EXISTS idx_chunks_hash ON chunks(hash);
"#;

/// Columns added after V1, applied by `migrate` to existing databases.
const V2_COLUMNS: &[(&str, &str)] = &[
    ("hash_reason", "TEXT"),
    ("volume_serial", "INTEGER"),
    ("file_index", "INTEGER"),
    ("chunk_count", "INTEGER NOT NULL DEFAULT 0"),
    ("opened_ts", "INTEGER NOT NULL DEFAULT 0"),
    // 0.3.0: evidence channel + strength drive every confidence figure.
    // Deliberately NOT indexed: they are only read per file row.
    ("source_evidence", "TEXT NOT NULL DEFAULT ''"),
    ("source_strength", "INTEGER NOT NULL DEFAULT 0"),
];

/// Bare column names on the right-hand side refer to the *pre-update* row,
/// which lets us invalidate the content fingerprint only when size/mtime
/// actually changed (metadata-only touches otherwise cost nothing).
///
/// A changed file goes to `settling`, not straight to `pending`: the hash
/// thread only promotes it once the write burst has gone quiet, so an
/// in-progress download is never digested repeatedly.
const UPSERT_SQL: &str = r#"
INSERT INTO files
    (path, name, ext, size, mtime, source_kind, source_label, source_detail,
     source_evidence, source_strength,
     hash_state, created_at, last_seen_at, change_count, liveness,
     volume_serial, file_index)
VALUES
    (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?12, ?13, 'settling', ?9, ?9, 0, 'live', ?10, ?11)
ON CONFLICT(path) DO UPDATE SET
    last_seen_at  = excluded.last_seen_at,
    liveness      = 'live',
    source_kind   = excluded.source_kind,
    source_label  = excluded.source_label,
    source_detail = excluded.source_detail,
    source_evidence = excluded.source_evidence,
    source_strength = excluded.source_strength,
    volume_serial = COALESCE(excluded.volume_serial, volume_serial),
    file_index    = COALESCE(excluded.file_index, file_index),
    content_hash  = CASE WHEN size <> excluded.size OR mtime <> excluded.mtime
                         THEN NULL ELSE content_hash END,
    hash_state    = CASE WHEN size <> excluded.size OR mtime <> excluded.mtime
                         THEN 'settling' ELSE hash_state END,
    hash_reason   = CASE WHEN size <> excluded.size OR mtime <> excluded.mtime
                         THEN NULL ELSE hash_reason END,
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

/// `%`/`_` in a user query must be escaped, otherwise searching `a_b` also
/// matches `axb`. Paired with `ESCAPE '\'` in the SQL.
fn like_pattern(query: &str) -> String {
    let escaped = query
        .to_lowercase()
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{}%", escaped)
}

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
       f.created_at, f.opened_ts, f.source_evidence, f.source_strength
  FROM files f
 WHERE (?1 = '' OR lower(f.name) LIKE ?2 ESCAPE '\' OR lower(f.path) LIKE ?2 ESCAPE '\')
   AND (?5 = '' OR f.source_kind = ?5)
 ORDER BY last_ts DESC
 LIMIT ?3 OFFSET ?4
"#;

/// Note on `ESCAPE '\'`: this string is a Rust raw string, so the SQL the
/// engine receives is literally `ESCAPE '\'` — a one-character backslash
/// escape clause, exactly what SQLite requires.

const COUNT_SQL: &str = r#"
SELECT COUNT(*) FROM files f
 WHERE (?1 = '' OR lower(f.name) LIKE ?2 ESCAPE '\' OR lower(f.path) LIKE ?2 ESCAPE '\')
   AND (?3 = '' OR f.source_kind = ?3)
"#;

const FILE_BY_ID_SQL: &str = r#"
SELECT f.id, f.path, f.name, f.ext, f.size, f.mtime,
       f.source_kind, f.source_label, f.source_detail,
       f.change_count, f.content_hash, f.hash_state, f.liveness,
       (SELECT COUNT(*) FROM files d
         WHERE d.content_hash IS NOT NULL
           AND d.content_hash = f.content_hash
           AND d.id <> f.id AND d.liveness = 'live'),
       COALESCE((SELECT MAX(e.ts) FROM events e WHERE e.file_id = f.id), f.created_at),
       f.created_at, f.opened_ts, f.source_evidence, f.source_strength
  FROM files f WHERE f.id = ?1
"#;

const EVENTS_SQL: &str = r#"
SELECT id, kind, ts, size, process_name, process_path, pid, attribution, note
  FROM events WHERE file_id = ?1 ORDER BY ts DESC, id DESC LIMIT ?2
"#;

/// Everything `get_status` needs, computed at most once per TTL. The status
/// endpoint is polled continuously, and each of these figures used to be a
/// full table scan (or a `COUNT(*)` over the million-row chunk table).
#[derive(Debug, Clone, Default)]
pub struct DbStats {
    pub counts: Counts,
    pub db_size: i64,
    pub total_db_size: i64,
    pub archived_events: i64,
    pub archive_files: i64,
}

pub struct Store {
    conn: Mutex<Connection>,
    pub db_path: PathBuf,
    archive_dir: PathBuf,
    /// Lazily opened read-only archive databases, keyed by file name.
    archives: Mutex<Option<Vec<(PathBuf, Connection)>>>,
    /// Directories the collector watches. A "unique copy" conclusion is only
    /// meaningful for files inside them.
    watch_dirs: Mutex<Vec<PathBuf>>,
    /// `(computed_at_ms, stats)` — see [`Store::stats`].
    stats_cache: Mutex<Option<(i64, DbStats)>>,
    /// Last non-fatal storage error, surfaced through `Status::last_error`.
    last_error: Mutex<Option<String>>,
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
        conn.execute_batch(SCHEMA_TABLES)?;
        migrate(&conn)?;
        conn.execute_batch(SCHEMA_INDEXES)?;

        let archive_dir = path
            .parent()
            .map(|p| p.join("archive"))
            .unwrap_or_else(|| PathBuf::from("archive"));
        let _ = std::fs::create_dir_all(&archive_dir);

        Ok(Self {
            conn: Mutex::new(conn),
            db_path: path.to_path_buf(),
            archive_dir,
            archives: Mutex::new(None),
            watch_dirs: Mutex::new(Vec::new()),
            stats_cache: Mutex::new(None),
            last_error: Mutex::new(None),
        })
    }

    /// Record the directories the collector watches. Called once at startup;
    /// used by the Q3 coverage rule ("唯一副本" only holds inside them).
    pub fn set_watch_dirs(&self, dirs: Vec<PathBuf>) {
        if let Ok(mut guard) = self.watch_dirs.lock() {
            *guard = dirs;
        }
    }

    fn is_watched(&self, path: &str) -> bool {
        let Ok(guard) = self.watch_dirs.lock() else {
            return false;
        };
        if guard.is_empty() {
            // The collector only ever inserts files it found under the watch
            // dirs; an empty list means "not configured yet", not "outside".
            return true;
        }
        let candidate = Path::new(path);
        guard.iter().any(|dir| candidate.starts_with(dir))
    }

    /// Persist a non-fatal storage error for the status endpoint.
    pub fn set_last_error(&self, message: impl Into<String>) {
        if let Ok(mut guard) = self.last_error.lock() {
            *guard = Some(message.into());
        }
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().ok().and_then(|guard| guard.clone())
    }

    /// Re-open the main connection. Used when a failed `DETACH` would leave the
    /// `arch` schema attached and make every later `ATTACH arch` fail — which
    /// silently stops archiving forever.
    fn reopen(&self) -> rusqlite::Result<()> {
        let conn = Connection::open(&self.db_path)?;
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        let _ = conn.pragma_update(None, "synchronous", "NORMAL");
        let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
        conn.execute_batch(SCHEMA_TABLES)?;
        migrate(&conn)?;
        conn.execute_batch(SCHEMA_INDEXES)?;
        let mut guard = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        *guard = conn;
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Size of the main database file plus its WAL/SHM companions.
    pub fn db_size(&self) -> i64 {
        let mut total = std::fs::metadata(&self.db_path)
            .map(|m| m.len() as i64)
            .unwrap_or(0);
        for suffix in ["-wal", "-shm"] {
            let companion = PathBuf::from(format!("{}{}", self.db_path.to_string_lossy(), suffix));
            total += std::fs::metadata(companion)
                .map(|m| m.len() as i64)
                .unwrap_or(0);
        }
        total
    }

    /// Total size of the main database plus every archive file.
    pub fn total_size(&self) -> i64 {
        let mut total = self.db_size();
        if let Ok(entries) = std::fs::read_dir(&self.archive_dir) {
            for entry in entries.flatten() {
                if let Ok(meta) = entry.metadata() {
                    total += meta.len() as i64;
                }
            }
        }
        total
    }

    /// Cached [`DbStats`] for the status poll. `ttl_ms` bounds how stale the
    /// numbers may be; the expensive aggregate queries run at most once per
    /// window instead of on every 2.5 s poll. Writers call
    /// [`Store::invalidate_stats`] on meaningful changes so the next poll
    /// recomputes immediately.
    pub fn stats(&self, ttl_ms: i64) -> DbStats {
        let now = now_ms();
        if let Ok(guard) = self.stats_cache.lock() {
            if let Some((computed_at, cached)) = guard.as_ref() {
                if now - *computed_at < ttl_ms {
                    return cached.clone();
                }
            }
        }
        let fresh = DbStats {
            counts: self.counts(),
            db_size: self.db_size(),
            total_db_size: self.total_size(),
            archived_events: self.archived_event_count(),
            archive_files: self.archive_file_count(),
        };
        if let Ok(mut guard) = self.stats_cache.lock() {
            *guard = Some((now, fresh.clone()));
        }
        fresh
    }

    pub fn invalidate_stats(&self) {
        if let Ok(mut guard) = self.stats_cache.lock() {
            *guard = None;
        }
    }

    // ------------------------------------------------------------- writing

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
                    obs.path,
                    obs.name,
                    obs.ext,
                    obs.size,
                    obs.mtime,
                    src.kind,
                    src.label,
                    src.detail,
                    now,
                    obs.volume,
                    obs.file_index,
                    src.evidence,
                    src.strength
                ])?;
                baseline.execute(params![obs.path, now, obs.size])?;
            }
        }
        tx.commit()?;
        self.invalidate_stats();
        Ok(items.len())
    }

    pub fn upsert_file(&self, obs: &FileObservation, src: &SourceInfo) -> rusqlite::Result<i64> {
        let conn = self.lock();
        let now = now_ms();
        conn.execute(
            UPSERT_SQL,
            params![
                obs.path,
                obs.name,
                obs.ext,
                obs.size,
                obs.mtime,
                src.kind,
                src.label,
                src.detail,
                now,
                obs.volume,
                obs.file_index,
                src.evidence,
                src.strength
            ],
        )?;
        conn.query_row(
            "SELECT id FROM files WHERE path = ?1",
            params![obs.path],
            |r| r.get(0),
        )
    }

    /// Reconnect a rename/move instead of inserting a second row.
    ///
    /// Windows' `notify` backend never emits `RenameMode::Both` (only `From`
    /// and `To`), macOS emits a single `Name(Any)` with just the new path, and
    /// Linux emits both a `Both` and extra single-path events. Relying on a
    /// two-path event therefore breaks lineage on two of three platforms. The
    /// file identity `(volume_serial, file_index)` is the portable signal: if a
    /// row with the same identity currently marked `gone` exists, the new path
    /// is *the same file* and only its path column changes.
    ///
    /// Returns `(id, previous_path)` when a row was reconnected.
    pub fn reconnect_identity(
        &self,
        obs: &FileObservation,
        src: &SourceInfo,
    ) -> Option<(i64, String)> {
        let (volume, index) = (obs.volume?, obs.file_index?);
        let conn = self.lock();
        let now = now_ms();
        // The `NOT EXISTS` guard also keeps the UNIQUE(path) constraint safe
        // when the destination path is already occupied by a live row.
        let previous: Option<(i64, String)> = conn
            .query_row(
                "SELECT id, path FROM files
                  WHERE volume_serial = ?1 AND file_index = ?2
                    AND path <> ?3 AND liveness = 'gone'
                    AND NOT EXISTS (SELECT 1 FROM files x WHERE x.path = ?3)
                  LIMIT 1",
                params![volume, index, obs.path],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .ok()
            .flatten();
        let (id, old_path) = previous?;
        let updated = conn.execute(
            "UPDATE files SET
                 path = ?1, name = ?2, ext = ?3, size = ?4, mtime = ?5,
                 source_kind = ?6, source_label = ?7, source_detail = ?8,
                 source_evidence = ?9, source_strength = ?10,
                 liveness = 'live', last_seen_at = ?11,
                 volume_serial = COALESCE(?12, volume_serial),
                 file_index = COALESCE(?13, file_index),
                 content_hash = CASE WHEN size <> ?4 OR mtime <> ?5
                                     THEN NULL ELSE content_hash END,
                 hash_state = CASE WHEN size <> ?4 OR mtime <> ?5
                                   THEN 'settling' ELSE hash_state END,
                 hash_reason = CASE WHEN size <> ?4 OR mtime <> ?5
                                    THEN NULL ELSE hash_reason END
               WHERE id = ?14",
            params![
                obs.path,
                obs.name,
                obs.ext,
                obs.size,
                obs.mtime,
                src.kind,
                src.label,
                src.detail,
                src.evidence,
                src.strength,
                now,
                obs.volume,
                obs.file_index,
                id
            ],
        );
        match updated {
            Ok(0) => None,
            Ok(_) => Some((id, old_path)),
            Err(error) => {
                self.set_last_error(format!("重命名重连失败：{}", error));
                None
            }
        }
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
        self.invalidate_stats();
        Ok(true)
    }

    /// Records an "opened" observation without touching `change_count`
    /// (opening a file is not modifying it).
    pub fn record_opened(&self, file_id: i64, ts: i64, note: &str) -> rusqlite::Result<bool> {
        let conn = self.lock();
        let previous: Option<i64> = conn
            .query_row(
                "SELECT opened_ts FROM files WHERE id = ?1",
                params![file_id],
                |r| r.get(0),
            )
            .optional()?;
        let previous = previous.unwrap_or(0);
        if ts <= previous {
            return Ok(false);
        }
        let already: Option<i64> = conn
            .query_row(
                "SELECT id FROM events WHERE file_id = ?1 AND kind = 'opened' AND ts = ?2 LIMIT 1",
                params![file_id, ts],
                |r| r.get(0),
            )
            .optional()?;
        if already.is_some() {
            conn.execute(
                "UPDATE files SET opened_ts = ?1 WHERE id = ?2",
                params![ts, file_id],
            )?;
            return Ok(false);
        }
        conn.execute(
            "INSERT INTO events (file_id, kind, ts, size, attribution, note)
             VALUES (?1, 'opened', ?2, 0, 'recent-lnk', ?3)",
            params![file_id, ts, note],
        )?;
        conn.execute(
            "UPDATE files SET opened_ts = ?1 WHERE id = ?2",
            params![ts, file_id],
        )?;
        Ok(true)
    }

    /// Record a downloader task as a real timeline event. Idempotent on
    /// `(file_id, kind='downloaded', ts)` so repeated rescans do not spam.
    pub fn record_download(&self, file_id: i64, ts: i64, note: &str) -> rusqlite::Result<bool> {
        let conn = self.lock();
        let already: Option<i64> = conn
            .query_row(
                "SELECT id FROM events
                  WHERE file_id = ?1 AND kind = 'downloaded' AND ts = ?2 LIMIT 1",
                params![file_id, ts],
                |r| r.get(0),
            )
            .optional()?;
        if already.is_some() {
            return Ok(false);
        }
        conn.execute(
            "INSERT INTO events (file_id, kind, ts, size, attribution, note)
             VALUES (?1, 'downloaded', ?2, 0, 'download-task', ?3)",
            params![file_id, ts, note],
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

    pub fn identity_for_path(&self, path: &str) -> Option<(Option<i64>, Option<i64>)> {
        let conn = self.lock();
        conn.query_row(
            "SELECT volume_serial, file_index FROM files WHERE path = ?1",
            params![path],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok()
    }

    /// Every tracked file with this exact name (case-insensitive), narrowed by
    /// extension and size when the caller knows them.
    ///
    /// Name-only matching is what made "open `/Downloads/report.pdf`" write an
    /// `opened` event onto *every* `report.pdf` in the database, so callers
    /// pass whatever the source record knows (`""`/`0` = unknown, keep).
    pub fn ids_by_name(&self, name: &str, ext: &str, size: i64) -> Vec<(i64, String)> {
        let conn = self.lock();
        let mut stmt = match conn.prepare(
            "SELECT id, path FROM files
              WHERE lower(name) = ?1 AND liveness = 'live'
                AND (?2 = '' OR ext = ?2)
                AND (?3 <= 0 OR size <= 0 OR size = ?3)
              LIMIT 200",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(
            params![name.to_lowercase(), ext.to_lowercase(), size],
            |r| Ok((r.get(0)?, r.get(1)?)),
        );
        match rows {
            Ok(rows) => rows.flatten().collect(),
            Err(_) => Vec::new(),
        }
    }

    // ----------------------------------------------------------- fingerprint

    /// Promote files whose write burst has gone quiet into the hash queue.
    pub fn promote_settled(&self, now: i64, settle_seconds: i64) -> rusqlite::Result<usize> {
        let conn = self.lock();
        let changed = conn.execute(
            "UPDATE files SET hash_state = 'pending'
              WHERE hash_state = 'settling'
                AND liveness = 'live'
                AND (?1 - mtime) >= ?2",
            params![now, settle_seconds.saturating_mul(1_000)],
        )?;
        Ok(changed)
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

    pub fn chunks_for(&self, file_id: i64) -> Vec<ChunkDigest> {
        let conn = self.lock();
        let mut stmt =
            match conn.prepare("SELECT idx, hash FROM chunks WHERE file_id = ?1 ORDER BY idx") {
                Ok(s) => s,
                Err(_) => return Vec::new(),
            };
        let rows = stmt.query_map(params![file_id], |r| {
            Ok(ChunkDigest {
                index: r.get::<_, i64>(0)? as u32,
                hash: r.get(1)?,
            })
        });
        match rows {
            Ok(rows) => rows.flatten().collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Replace a file's chunk digests and content root in one transaction.
    pub fn set_digest(
        &self,
        file_id: i64,
        root: &str,
        chunks: &[ChunkDigest],
        incremental: bool,
    ) -> rusqlite::Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        {
            tx.execute("DELETE FROM chunks WHERE file_id = ?1", params![file_id])?;
            {
                let mut insert = tx.prepare_cached(
                    "INSERT OR REPLACE INTO chunks (file_id, idx, hash) VALUES (?1, ?2, ?3)",
                )?;
                for chunk in chunks {
                    insert.execute(params![file_id, chunk.index as i64, chunk.hash])?;
                }
            }
            tx.execute(
                "UPDATE files SET content_hash = ?1, hash_state = 'done',
                        hash_reason = ?2, chunk_count = ?3
                  WHERE id = ?4",
                params![
                    root,
                    if incremental { "incremental" } else { "full" },
                    chunks.len() as i64,
                    file_id
                ],
            )?;
        }
        tx.commit()
    }

    pub fn set_hash_state(&self, id: i64, state: &str) -> rusqlite::Result<()> {
        self.lock().execute(
            "UPDATE files SET hash_state = ?1 WHERE id = ?2",
            params![state, id],
        )?;
        Ok(())
    }

    /// Live files still waiting for a full digest. Q3 uses this as the
    /// "coverage" denominator: with unfinished fingerprints in the database, an
    /// "only copy" conclusion is not safe, so the answer must degrade.
    pub fn pending_hash_count(&self) -> i64 {
        self.lock()
            .query_row(
                "SELECT COUNT(*) FROM files
                  WHERE hash_state IN ('pending','settling','error') AND liveness = 'live'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0)
    }

    /// Total events (live + archived) and distinct attributed processes for one
    /// file. Used so Q2's "N 个进程碰过它" is not computed from a truncated
    /// 500-row page.
    pub fn event_totals(&self, file_id: i64) -> (i64, i64) {
        let mut names: Vec<String> = Vec::new();
        let mut total = 0i64;
        {
            let conn = self.lock();
            total += conn
                .query_row(
                    "SELECT COUNT(*) FROM events WHERE file_id = ?1",
                    params![file_id],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap_or(0);
            if let Ok(mut stmt) = conn.prepare(
                "SELECT DISTINCT process_name FROM events
                  WHERE file_id = ?1 AND process_name IS NOT NULL",
            ) {
                if let Ok(rows) = stmt.query_map(params![file_id], |r| r.get::<_, String>(0)) {
                    for name in rows.flatten() {
                        if !names.contains(&name) {
                            names.push(name);
                        }
                    }
                }
            };
            // The trailing semicolon matters: as a block-trailing expression
            // the if-let's temporary `Result<Statement<'_>>` would outlive the
            // `conn` guard (E0597).
        }
        let archived_total = self.with_archives(|list| {
            let mut count = 0i64;
            for (_, conn) in list {
                count += conn
                    .query_row(
                        "SELECT COUNT(*) FROM events WHERE file_id = ?1",
                        params![file_id],
                        |r| r.get::<_, i64>(0),
                    )
                    .unwrap_or(0);
                if let Ok(mut stmt) = conn.prepare(
                    "SELECT DISTINCT process_name FROM events
                      WHERE file_id = ?1 AND process_name IS NOT NULL",
                ) {
                    if let Ok(rows) = stmt.query_map(params![file_id], |r| r.get::<_, String>(0)) {
                        for name in rows.flatten() {
                            if !names.contains(&name) {
                                names.push(name);
                            }
                        }
                    }
                }
            }
            count
        });
        (total + archived_total, names.len() as i64)
    }

    pub fn schema_version(&self) -> i64 {
        self.meta_i64("schema_version", 0)
    }

    pub fn hash_algo_version(&self) -> i64 {
        self.meta_i64("hash_algo_version", 0)
    }

    fn meta_i64(&self, key: &str, fallback: i64) -> i64 {
        self.lock()
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![key],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten()
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(fallback)
    }

    // --------------------------------------------------------------- reading

    pub fn counts(&self) -> Counts {
        let conn = self.lock();
        conn.query_row(
            "SELECT
               (SELECT COUNT(*) FROM files),
               (SELECT COUNT(*) FROM events),
               (SELECT COUNT(*) FROM files WHERE hash_state = 'done'),
               (SELECT COUNT(*) FROM files WHERE hash_state IN ('pending','settling') AND liveness = 'live'),
               (SELECT COUNT(*) FROM files WHERE volume_serial IS NOT NULL),
               (SELECT COUNT(*) FROM chunks),
               (SELECT COUNT(*) FROM files WHERE hash_reason = 'incremental')",
            [],
            |r| {
                Ok(Counts {
                    files: r.get(0)?,
                    events: r.get(1)?,
                    hashed: r.get(2)?,
                    pending: r.get(3)?,
                    identified: r.get(4)?,
                    chunks: r.get(5)?,
                    incremental: r.get(6)?,
                })
            },
        )
        .unwrap_or_default()
    }

    /// `kind` filters server-side (`""` = all) so a filter chip can never show
    /// an empty list while matching rows exist further down the table.
    pub fn list_files(&self, query: &str, kind: &str, limit: i64, offset: i64) -> Vec<FileRow> {
        let conn = self.lock();
        let mut stmt = match conn.prepare(LIST_SQL) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let pattern = like_pattern(query);
        let rows = stmt.query_map(
            params![query, pattern, limit, offset, kind],
            row_to_file,
        );
        match rows {
            Ok(rows) => rows.flatten().collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Total rows matching the same filter as [`Store::list_files`].
    pub fn count_files(&self, query: &str, kind: &str) -> i64 {
        let conn = self.lock();
        conn.query_row(
            COUNT_SQL,
            params![query, like_pattern(query), kind],
            |r| r.get(0),
        )
        .unwrap_or(0)
    }

    /// Live events plus the same file's archived events, newest first.
    pub fn events_for_file(&self, id: i64) -> Vec<EventRow> {
        let mut events = self.events_live(id, 500);
        events.extend(self.events_archived(id, 500));
        events.sort_by(|a, b| b.ts.cmp(&a.ts).then(b.id.cmp(&a.id)));
        events.truncate(500);
        events
    }

    fn events_live(&self, id: i64, limit: i64) -> Vec<EventRow> {
        let conn = self.lock();
        let mut stmt = match conn.prepare(EVENTS_SQL) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(params![id, limit], row_to_event);
        match rows {
            Ok(rows) => rows.flatten().collect(),
            Err(_) => Vec::new(),
        }
    }

    fn events_archived(&self, id: i64, limit: i64) -> Vec<EventRow> {
        self.with_archives(|list| {
            let mut out = Vec::new();
            for (_, conn) in list {
                if let Ok(mut stmt) = conn.prepare(EVENTS_SQL) {
                    if let Ok(rows) = stmt.query_map(params![id, limit], row_to_event) {
                        out.extend(rows.flatten());
                    }
                }
            }
            out
        })
    }

    /// Run `f` with every archive database opened read-only. The handles are
    /// cached until the next archiving pass invalidates them.
    fn with_archives<T>(&self, f: impl FnOnce(&[(PathBuf, Connection)]) -> T) -> T {
        let mut guard = self.archives.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            *guard = Some(open_archives(&self.archive_dir));
        }
        let list: &[(PathBuf, Connection)] = match guard.as_ref() {
            Some(list) => list.as_slice(),
            None => &[],
        };
        f(list)
    }

    pub fn archive_file_count(&self) -> i64 {
        std::fs::read_dir(&self.archive_dir)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|e| {
                        e.path().extension().map(|x| x.eq_ignore_ascii_case("db")) == Some(true)
                    })
                    .count() as i64
            })
            .unwrap_or(0)
    }

    pub fn archived_event_count(&self) -> i64 {
        self.with_archives(|list| {
            list.iter()
                .map(|(_, conn)| {
                    conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get::<_, i64>(0))
                        .unwrap_or(0)
                })
                .sum()
        })
    }

    /// Content duplicates (whole-file) and hard links (same FileID).
    pub fn copies(
        &self,
        hash: &str,
        volume: Option<i64>,
        index: Option<i64>,
        exclude: i64,
    ) -> Vec<CopyRow> {
        let conn = self.lock();
        let mut stmt = match conn.prepare(
            "SELECT id, path, size, mtime,
                    CASE WHEN ?2 IS NOT NULL AND volume_serial = ?2
                              AND file_index = ?3
                         THEN 1 ELSE 0 END AS hardlink
               FROM files
              WHERE id <> ?4 AND liveness = 'live'
                AND (
                     (content_hash IS NOT NULL AND content_hash = ?1)
                  OR (?2 IS NOT NULL AND volume_serial = ?2 AND file_index = ?3)
                )
              ORDER BY hardlink DESC, path
              LIMIT 100",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(params![hash, volume, index, exclude], |r| {
            Ok(CopyRow {
                id: r.get(0)?,
                path: r.get(1)?,
                size: r.get(2)?,
                mtime: r.get(3)?,
                hardlink: r.get::<_, i64>(4)? != 0,
            })
        });
        match rows {
            Ok(rows) => rows.flatten().collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Files that share at least `min_shared` 1 MiB chunks with this one —
    /// the "same project / partially identical" signal.
    pub fn related_files(&self, file_id: i64, min_shared: i64, limit: i64) -> Vec<RelatedRow> {
        let conn = self.lock();
        let mut stmt = match conn.prepare(
            "SELECT f.id, f.path, f.size, COUNT(*) AS shared
               FROM chunks c
               JOIN chunks o ON o.hash = c.hash AND o.file_id <> c.file_id
               JOIN files f ON f.id = o.file_id AND f.liveness = 'live'
              WHERE c.file_id = ?1
              GROUP BY f.id, f.path, f.size
             HAVING COUNT(*) >= ?2
              ORDER BY shared DESC, f.size DESC
              LIMIT ?3",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(params![file_id, min_shared, limit], |r| {
            Ok(RelatedRow {
                id: r.get(0)?,
                path: r.get(1)?,
                size: r.get(2)?,
                shared_chunks: r.get(3)?,
                total_chunks: 0,
            })
        });
        match rows {
            Ok(rows) => rows.flatten().collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn file_row(&self, id: i64) -> Option<FileRow> {
        let conn = self.lock();
        let mut stmt = conn.prepare(FILE_BY_ID_SQL).ok()?;
        stmt.query_row(params![id], row_to_file).ok()
    }

    pub fn file_detail(&self, id: i64, ctx: &verdict::VerdictContext) -> Option<FileDetail> {
        let file = self.file_row(id)?;
        let versions = self.events_for_file(id);
        let (volume, index) = self.identity_for_path(&file.path).unwrap_or((None, None));
        let copies = match &file.content_hash {
            Some(hash) => self.copies(hash, volume, index, id),
            None => Vec::new(),
        };
        let mut related = self.related_files(id, 2, 8);
        let chunk_count = self.chunks_for(id).len();
        for row in related.iter_mut() {
            row.total_chunks = chunk_count as i64;
        }
        let processes = summarize_processes(&versions);
        let (total_events, distinct_processes) = self.event_totals(id);
        let pending_unhashed = self.pending_hash_count();
        let watched = self.is_watched(&file.path);
        let safety = verdict::SafetyDigest {
            hash_state: file.hash_state.clone(),
            real_copies: copies.iter().filter(|c| !c.hardlink).count() as i64,
            hardlink_copies: copies.iter().filter(|c| c.hardlink).count() as i64,
            pending_unhashed,
            watched,
            related: related.len() as i64,
        };

        let answers = build_answers(
            &file,
            &versions,
            &copies,
            &related,
            &processes,
            total_events,
            distinct_processes,
            ctx,
            &safety,
        );
        let verdict = build_verdict(&file, &processes, ctx, &safety);
        let graph = build_graph(&file, &versions, &copies);
        Some(FileDetail {
            file,
            versions,
            copies,
            related,
            graph,
            answers,
            verdict,
        })
    }

    // -------------------------------------------------------------- archiving

    /// Move every event older than `cutoff_ms` into per-month read-only files.
    /// Returns the number of rows moved.
    pub fn archive_events(&self, cutoff_ms: i64) -> rusqlite::Result<i64> {
        let conn = self.lock();
        let bounds: (Option<i64>, Option<i64>) = conn.query_row(
            "SELECT MIN(ts), MAX(ts) FROM events WHERE ts < ?1",
            params![cutoff_ms],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let (Some(min_ts), Some(max_ts)) = bounds else {
            return Ok(0);
        };

        let mut moved = 0i64;
        let mut touched: Vec<PathBuf> = Vec::new();
        let mut detach_broken = false;
        let mut cursor = min_ts;
        let mut rounds = 0;
        while cursor <= max_ts && rounds < 600 {
            rounds += 1;
            let (start, end, month) = month_bounds_ms(cursor);
            let upper = end.min(cutoff_ms);
            if upper > start {
                let archive_path = self.archive_dir.join(format!("archive-{}.db", month));
                let (count, detach_failed) = archive_month(&conn, &archive_path, start, upper);
                if let Some(count) = count {
                    moved += count;
                    if !touched.contains(&archive_path) {
                        touched.push(archive_path);
                    }
                }
                detach_broken |= detach_failed;
            }
            cursor = end;
        }

        drop(conn);
        if detach_broken {
            // A failed DETACH leaves the `arch` schema attached, which makes
            // every later `ATTACH arch` fail and stops archiving silently.
            // Rebuilding the connection is the only reliable recovery, so it is
            // done here and reported through `last_error`.
            self.set_last_error(
                "归档库 DETACH 失败，已重建数据库连接；本次归档可能不完整".to_string(),
            );
            if let Err(error) = self.reopen() {
                self.set_last_error(format!("归档连接重建失败：{}", error));
            }
            if let Ok(mut cache) = self.archives.lock() {
                *cache = None;
            }
        }

        if moved > 0 {
            // Index creation is done on the archive file directly: an index on
            // an ATTACHed schema has to be qualified in a way that is easy to
            // get wrong, and this path only runs during maintenance.
            for path in &touched {
                ensure_archive_index(path);
            }
            let conn = self.lock();
            // A failed checkpoint is harmless (WAL grows, next pass retries),
            // but a failed `last_archive_at` write means the UI keeps saying
            // "never archived" — log both instead of discarding.
            if let Err(error) = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(())) {
                crate::startup::log(&format!("WAL checkpoint 失败（无害，下次重试）：{}", error));
            }
            if let Err(error) = conn.execute(
                "INSERT INTO meta (key, value) VALUES ('last_archive_at', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![now_ms().to_string()],
            ) {
                crate::startup::log(&format!("写入 last_archive_at 失败：{}", error));
            }
            drop(conn);
            if let Ok(mut cache) = self.archives.lock() {
                *cache = None;
            }
        }
        self.invalidate_stats();
        Ok(moved)
    }
}

fn open_archives(dir: &Path) -> Vec<(PathBuf, Connection)> {
    let mut opened = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return opened;
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e.eq_ignore_ascii_case("db")) == Some(true))
        .collect();
    paths.sort();
    for path in paths {
        if let Ok(conn) = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
            opened.push((path, conn));
        }
    }
    opened
}

fn ensure_archive_index(path: &Path) {
    if let Ok(conn) = Connection::open(path) {
        let _ = conn
            .execute_batch("CREATE INDEX IF NOT EXISTS idx_arch_file ON events(file_id, ts DESC);");
    }
}

/// Copy one month slice into `archive_path` and delete it from the live table.
fn archive_month(
    conn: &Connection,
    archive_path: &Path,
    start_ms: i64,
    end_ms: i64,
) -> (Option<i64>, bool) {
    let attach = conn
        .execute(
            "ATTACH DATABASE ?1 AS arch",
            params![archive_path.to_string_lossy().to_string()],
        )
        .is_ok();
    if !attach {
        return (None, false);
    }

    let result = (|| -> rusqlite::Result<i64> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS arch.events (
                 id            INTEGER PRIMARY KEY,
                 file_id       INTEGER NOT NULL,
                 kind          TEXT    NOT NULL,
                 ts            INTEGER NOT NULL,
                 size          INTEGER NOT NULL DEFAULT 0,
                 process_name  TEXT,
                 process_path  TEXT,
                 pid           INTEGER,
                 attribution   TEXT    NOT NULL DEFAULT 'unknown',
                 note          TEXT
             );",
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO arch.events
                (id, file_id, kind, ts, size, process_name, process_path, pid, attribution, note)
             SELECT id, file_id, kind, ts, size, process_name, process_path, pid, attribution, note
               FROM main.events WHERE ts >= ?1 AND ts < ?2",
            params![start_ms, end_ms],
        )?;
        let deleted = conn.execute(
            "DELETE FROM main.events WHERE ts >= ?1 AND ts < ?2",
            params![start_ms, end_ms],
        )?;
        Ok(deleted as i64)
    })();

    // A failed DETACH must never be swallowed: the `arch` name stays attached
    // and every later `ATTACH arch` fails, stopping archiving for good.
    let detach_failed = conn.execute("DETACH DATABASE arch", []).is_err();
    (result.ok(), detach_failed)
}

fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let existing: Vec<String> = {
        let mut stmt = conn.prepare("PRAGMA table_info(files)")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
        rows.flatten().collect()
    };
    for (name, definition) in V2_COLUMNS {
        if !existing.iter().any(|c| c == name) {
            conn.execute(
                &format!("ALTER TABLE files ADD COLUMN {} {}", name, definition),
                [],
            )?;
        }
    }

    // The fingerprint scheme has its own version. It used to be conflated with
    // `schema_version`, which meant a pure schema bump looked like an algorithm
    // change (and vice versa). V1 stored a whole-file BLAKE3; the content root
    // here is BLAKE3 over the chunk digests, so every inherited V1 hash is a
    // different value for the same bytes and must be recomputed before it can be
    // compared.
    let algorithm: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'hash_algo_version'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if algorithm.as_deref() != Some(HASH_ALGO_VERSION) {
        conn.execute(
            "UPDATE files
                SET content_hash = NULL, hash_state = 'pending', hash_reason = NULL, chunk_count = 0
              WHERE chunk_count = 0",
            [],
        )?;
    }

    conn.execute(
        "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![SCHEMA_VERSION.to_string()],
    )?;
    conn.execute(
        "INSERT INTO meta (key, value) VALUES ('hash_algo_version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![HASH_ALGO_VERSION],
    )?;
    Ok(())
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
        opened_ts: r.get(16)?,
        source_evidence: r.get(17)?,
        source_strength: r.get(18)?,
    })
}

fn row_to_event(r: &rusqlite::Row<'_>) -> rusqlite::Result<EventRow> {
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
}

/// Distinct-process summary for Q2, built once per detail request.
///
/// The three evidence kinds are kept apart on purpose: an ETW write, a live
/// write handle, and a downloader task log say very different things, and the
/// old code merged the last one into "N 个进程碰过它" even though that producer
/// is a historical tool name, not a live process.
#[derive(Debug, Default)]
struct ProcessSummary {
    etw_hits: i64,
    live: Vec<(String, usize)>,
    historical: Vec<(String, usize)>,
    inferred: i64,
    opened_ts: i64,
}

impl ProcessSummary {
    fn digest(&self, ctx: &verdict::VerdictContext) -> verdict::ProcessDigest {
        verdict::ProcessDigest {
            etw_hits: self.etw_hits,
            live: self.live.len() as i64,
            historical: self.historical.len() as i64,
            inferred: self.inferred,
            etw_available: ctx.etw_available,
        }
    }
}

fn bump(list: &mut Vec<(String, usize)>, name: &str) {
    match list.iter_mut().find(|(existing, _)| existing == name) {
        Some(entry) => entry.1 += 1,
        None => list.push((name.to_string(), 1)),
    }
}

fn summarize_processes(versions: &[EventRow]) -> ProcessSummary {
    let mut summary = ProcessSummary::default();
    for ev in versions {
        if ev.kind == "opened" {
            if ev.ts > summary.opened_ts {
                summary.opened_ts = ev.ts;
            }
            if ev.attribution == "recent-lnk" {
                summary.inferred += 1;
            }
            continue;
        }
        let Some(process) = &ev.process_name else {
            continue;
        };
        match ev.attribution.as_str() {
            "etw" => {
                summary.etw_hits += 1;
                bump(&mut summary.live, process);
            }
            "download-task" => bump(&mut summary.historical, process),
            "recent-lnk" => summary.inferred += 1,
            _ => bump(&mut summary.live, process),
        }
    }
    summary.live.sort_by(|a, b| b.1.cmp(&a.1));
    summary.historical.sort_by(|a, b| b.1.cmp(&a.1));
    summary
}

fn join_names(list: &[(String, usize)]) -> String {
    list.iter()
        .take(5)
        .map(|(name, count)| format!("{}（{} 次）", name, count))
        .collect::<Vec<_>>()
        .join("、")
}

#[allow(clippy::too_many_arguments)]
fn build_answers(
    file: &FileRow,
    versions: &[EventRow],
    copies: &[CopyRow],
    related: &[RelatedRow],
    processes: &ProcessSummary,
    total_events: i64,
    distinct_processes: i64,
    ctx: &verdict::VerdictContext,
    safety: &verdict::SafetyDigest,
) -> Answers {
    // ---- Q1: where did it come from -----------------------------------------
    let origin_headline = match file.source_kind.as_str() {
        "download" => "来自外部下载",
        "email" => "来自邮件",
        "im" => "来自聊天工具",
        "local" => "本机生成",
        _ => "来源未知",
    };
    // Confidence comes from the evidence strength, never from the kind alone:
    // "the path contains \Downloads\" used to be reported as high confidence.
    let origin_conf = verdict::origin_confidence(&file.source_kind, file.source_strength);
    let mut origin_detail = file.source_detail.clone().unwrap_or_else(|| {
        "没有找到任何来源记录。可能是很早以前就存在的文件，或由未接入适配器的数据源产生。".into()
    });
    if file.source_strength <= 2 {
        origin_detail.push_str(&format!(
            "\n证据通道：{}（强度 {}），因此来源结论最高只能给到「{}」置信。",
            if file.source_evidence.is_empty() {
                "无"
            } else {
                file.source_evidence.as_str()
            },
            file.source_strength,
            origin_conf
        ));
    }

    // ---- Q2: who touched it -------------------------------------------------
    let mod_conf = verdict::modifiers_confidence(&processes.digest(ctx));
    let (mod_headline, mut mod_detail) = if !processes.live.is_empty() {
        let channel = if processes.etw_hits > 0 {
            "ETW 内核事件流（真·写入 PID）"
        } else {
            "Restart Manager 写句柄（只证明写入者）"
        };
        (
            format!(
                "{} 个进程碰过它",
                distinct_processes.max(processes.live.len() as i64)
            ),
            format!(
                "活进程写入者（按事件次数排序）：{}。\n主要证据来自{}。\n统计口径：全库共 {} 条事件；时间线只展示最近 {} 条。",
                join_names(&processes.live),
                channel,
                total_events,
                versions.len()
            ),
        )
    } else if !processes.historical.is_empty() {
        (
            "未捕获到活进程，但已确认产生者".to_string(),
            format!(
                "产生者（历史记录，不是当前活进程）：{}。\n\
                 这类证据来自下载器任务日志：它能说明文件由哪个工具产生，但不能说明现在还有谁在动它。\n\
                 统计口径：全库共 {} 条事件。",
                join_names(&processes.historical),
                total_events
            ),
        )
    } else {
        let why = if !ctx.platform_supports_etw {
            "本平台没有 ETW / Restart Manager 等价通道，进程级归因不可用。"
        } else if !ctx.etw_available {
            "ETW 未启用（未提权或被配置关闭），句柄探测也没有抓到写入者。"
        } else {
            "写入太快、进程已关闭句柄，或文件由扫描登记。"
        };
        (
            "尚未捕获到写入进程".to_string(),
            format!(
                "文件出现或变化时没有拿到归属进程。{}\n统计口径：全库共 {} 条事件。",
                why, total_events
            ),
        )
    };
    if processes.opened_ts > 0 {
        mod_detail.push_str(&format!(
            "\nShell「最近使用」记录显示它在 {} 被打开过（按文件名匹配，属推断信号）。",
            human_relative(processes.opened_ts)
        ));
    }

    // ---- Q3: would deleting it lose anything --------------------------------
    let fingerprint: String = file
        .content_hash
        .as_deref()
        .map(|h| h.chars().take(16).collect())
        .unwrap_or_default();
    let hardlinks: Vec<&CopyRow> = copies.iter().filter(|c| c.hardlink).collect();
    let real_copies: Vec<&CopyRow> = copies.iter().filter(|c| !c.hardlink).collect();
    let safety_conf = verdict::safety_confidence(safety);

    let related_note = if related.is_empty() {
        String::new()
    } else {
        format!(
            "\n另有 {} 个文件与它共享部分内容块（最多 {} 块），属于关联文件，不是副本。",
            related.len(),
            related.first().map(|r| r.shared_chunks).unwrap_or(0)
        )
    };

    let (safety_headline, safety_detail) = if !safety.ready() {
        (
            "内容指纹尚未生成".to_string(),
            format!(
                "后台哈希队列状态：{}。完成后才能判断是否存在相同内容的其他副本。",
                file.hash_state
            ),
        )
    } else if !real_copies.is_empty() {
        let mut lines: Vec<String> = Vec::new();
        lines.push(format!("内容完全相同的独立副本 {} 个：", real_copies.len()));
        for copy in real_copies.iter().take(4) {
            lines.push(format!("  · {}", copy.path));
        }
        if real_copies.len() > 4 {
            lines.push(format!("  · …另有 {} 处", real_copies.len() - 4));
        }
        if !hardlinks.is_empty() {
            lines.push(format!("另有 {} 个硬链接指向同一份数据。", hardlinks.len()));
        }
        (
            format!("还有 {} 个副本，删除不会丢内容", real_copies.len()),
            format!(
                "BLAKE3 指纹 {}…：\n{}{}",
                fingerprint,
                lines.join("\n"),
                related_note
            ),
        )
    } else if !hardlinks.is_empty() {
        let mut lines: Vec<String> = vec![format!(
            "硬链接 {} 个（同一个 FileID，磁盘上只有一份数据）：",
            hardlinks.len()
        )];
        for link in hardlinks.iter().take(4) {
            lines.push(format!("  · {}", link.path));
        }
        (
            format!(
                "只是 {} 个硬链接，删掉不释放空间也不丢内容",
                hardlinks.len()
            ),
            format!("{}{}", lines.join("\n"), related_note),
        )
    } else if safety.coverage_ok() {
        // "Only copy" is only claimable when the fingerprint queue is empty:
        // otherwise the very file that would have matched may simply not have
        // been hashed yet.
        let extra = if file.source_strength >= 3 {
            "\n如果原始下载地址仍然有效，也可以重新获取该文件。"
        } else {
            ""
        };
        (
            "唯一副本，删除会丢内容".to_string(),
            format!(
                "在已监控目录中，BLAKE3 指纹 {}… 只出现一次（{}），且库内没有未完成指纹的文件。删除后本机不再有相同内容。{}{}",
                fingerprint,
                human_size(file.size),
                extra,
                related_note
            ),
        )
    } else {
        (
            "可能是唯一副本，但指纹覆盖不完整".to_string(),
            format!(
                "在已监控目录中，BLAKE3 指纹 {}… 暂时没有匹配到副本；但库内仍有 {} 个文件未完成指纹，其中可能包含相同内容。建议等指纹队列清空后再判断。{}",
                fingerprint,
                safety.pending_unhashed,
                related_note
            ),
        )
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
            confidence: mod_conf.to_string(),
        },
        safety: Answer {
            headline: safety_headline,
            detail: safety_detail,
            confidence: safety_conf.to_string(),
        },
    }
}

/// The combined conclusion. Pure delegation to [`verdict::build`] so the rules
/// stay testable outside the database layer.
fn build_verdict(
    file: &FileRow,
    processes: &ProcessSummary,
    ctx: &verdict::VerdictContext,
    safety: &verdict::SafetyDigest,
) -> Verdict {
    verdict::build(&verdict::VerdictInput {
        source_kind: file.source_kind.clone(),
        source_strength: file.source_strength,
        source_evidence: file.source_evidence.clone(),
        processes: processes.digest(ctx),
        safety: safety.clone(),
        platform_supports_etw: ctx.platform_supports_etw,
    })
}

fn human_relative(ts: i64) -> String {
    let now = now_ms();
    let diff = now - ts;
    if diff < 0 {
        return format!("{}", ts);
    }
    let minutes = diff / 60_000;
    if minutes < 60 {
        format!("{} 分钟前", minutes.max(0))
    } else if minutes < 60 * 24 {
        format!("{} 小时前", minutes / 60)
    } else {
        format!("{} 天前", minutes / (60 * 24))
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

    // `opened` events are a different axis from content versions, so they are
    // folded into the version chain with their own label.
    //
    // `versions` is newest-first; taking the *first* 12 and then reversing in
    // place yields the most recent 12 in chronological order. (`rev().take(12)`
    // — the previous code — silently selected the twelve *oldest* events.)
    let chronological: Vec<&EventRow> = versions.iter().take(12).rev().collect();
    let mut prev: Option<String> = None;
    let mut process_ids: Vec<String> = Vec::new();
    for (i, ev) in chronological.iter().enumerate() {
        let node = format!("ver:{}", ev.id);
        graph.nodes.push(GraphNode {
            id: node.clone(),
            label: format!("v{}", i + 1),
            kind: "version".into(),
            sub: Some(event_label(&ev.kind)),
            weight: 1.0,
        });
        if let Some(previous) = &prev {
            graph.edges.push(GraphEdge {
                from: previous.clone(),
                to: node.clone(),
                label: "演进".into(),
                kind: "evolve".into(),
            });
        }
        if let Some(process) = &ev.process_name {
            let pid = format!("proc:{}", process.to_lowercase());
            if !process_ids.contains(&pid) {
                process_ids.push(pid.clone());
                graph.nodes.push(GraphNode {
                    id: pid.clone(),
                    label: process.clone(),
                    kind: "process".into(),
                    sub: Some(if ev.attribution == "etw" {
                        "ETW 内核事件".into()
                    } else if ev.attribution == "download-task" {
                        "下载器任务记录（历史产生者）".into()
                    } else {
                        "持有写句柄".into()
                    }),
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
    if let Some(previous) = prev {
        graph.edges.push(GraphEdge {
            from: previous,
            to: file_id.clone(),
            label: "当前版本".into(),
            kind: "evolve".into(),
        });
    }

    for copy in copies.iter().take(10) {
        let node = format!("copy:{}", copy.id);
        graph.nodes.push(GraphNode {
            id: node.clone(),
            label: Path::new(&copy.path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| copy.path.clone()),
            kind: "copy".into(),
            sub: Some(copy.path.clone()),
            weight: 0.9,
        });
        graph.edges.push(GraphEdge {
            from: file_id.clone(),
            to: node,
            label: if copy.hardlink {
                "硬链接".into()
            } else {
                "内容相同".into()
            },
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
        "opened" => "被打开".into(),
        "downloaded" => "下载".into(),
        other => other.to_string(),
    }
}
