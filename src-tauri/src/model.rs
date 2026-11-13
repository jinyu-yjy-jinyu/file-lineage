use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileRow {
    pub id: i64,
    pub path: String,
    pub name: String,
    pub ext: String,
    pub size: i64,
    pub mtime: i64,
    pub source_kind: String,
    pub source_label: String,
    pub source_detail: Option<String>,
    pub change_count: i64,
    pub created_at: i64,
    pub duplicate_count: i64,
    pub content_hash: Option<String>,
    pub hash_state: String,
    pub last_event_ts: i64,
    pub liveness: String,
    /// Newest shell "Recent" timestamp, 0 when the file was never opened.
    pub opened_ts: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventRow {
    pub id: i64,
    pub kind: String,
    pub ts: i64,
    pub size: i64,
    pub process_name: Option<String>,
    pub process_path: Option<String>,
    pub pid: Option<i64>,
    pub attribution: String,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyRow {
    pub id: i64,
    pub path: String,
    pub size: i64,
    pub mtime: i64,
    /// Same `(volume, file index)` as the file itself: a hard link, i.e. the
    /// very same data reachable through another name. Deleting it frees nothing.
    pub hardlink: bool,
}

/// A file that shares part of its 1 MiB chunk digests — "same project", not a copy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelatedRow {
    pub id: i64,
    pub path: String,
    pub size: i64,
    pub shared_chunks: i64,
    pub total_chunks: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Counts {
    pub files: i64,
    pub events: i64,
    pub hashed: i64,
    pub pending: i64,
    /// Files whose `(volume, file index)` identity is known.
    pub identified: i64,
    pub chunks: i64,
    pub incremental: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphNode {
    pub id: String,
    pub label: String,
    pub kind: String,
    pub sub: Option<String>,
    pub weight: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    pub label: String,
    pub kind: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Graph {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Answer {
    /// One-line verdict, e.g. "浏览器下载".
    pub headline: String,
    /// Human readable evidence.
    pub detail: String,
    /// high | medium | low
    pub confidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Answers {
    /// Q1 — where did it come from.
    pub origin: Answer,
    /// Q2 — who touched it.
    pub modifiers: Answer,
    /// Q3 — would deleting it lose anything.
    pub safety: Answer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDetail {
    pub file: FileRow,
    pub versions: Vec<EventRow>,
    pub copies: Vec<CopyRow>,
    pub related: Vec<RelatedRow>,
    pub graph: Graph,
    pub answers: Answers,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub watching: bool,
    pub dirs: Vec<String>,
    pub file_count: i64,
    pub event_count: i64,
    pub hashed_count: i64,
    pub pending_hash: i64,
    /// idle | scanning | hashing | ready
    pub scan_state: String,
    pub scan_total: i64,
    pub scan_done: i64,
    pub events_this_session: i64,
    pub attribution_hits: i64,
    pub attribution_misses: i64,
    pub db_path: String,
    pub db_size: i64,
    pub started_at: i64,
    pub last_event_at: i64,
    pub last_error: Option<String>,
    pub adapters: Vec<String>,
    pub source_records: i64,

    // ---- V2: collection hardening -----------------------------------------
    /// Is this process running with an elevated token?
    pub elevated: bool,
    /// `disabled` | `active` | `denied` | `error: …`
    pub etw_state: String,
    /// Events attributed by ETW rather than Restart Manager this session.
    pub etw_events: i64,
    pub autostart: bool,
    pub autostart_command: Option<String>,
    pub config_path: String,
    pub archived_events: i64,
    pub archive_files: i64,
    pub last_archive_at: i64,
    pub identified_files: i64,
    pub chunk_count: i64,
    pub incremental_hashes: i64,
    pub recent_entries: i64,
    pub total_db_size: i64,
    pub schema_version: i64,
}
