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
}
