export interface FileRow {
  id: number;
  path: string;
  name: string;
  ext: string;
  size: number;
  mtime: number;
  sourceKind: string;
  sourceLabel: string;
  sourceDetail: string | null;
  /** Evidence channel id, e.g. `zone.ads` / `path.shape` / `rm.handle`. */
  sourceEvidence: string;
  /** 3 objective · 2 strong indirect · 1 heuristic · 0 none */
  sourceStrength: number;
  changeCount: number;
  createdAt: number;
  duplicateCount: number;
  contentHash: string | null;
  hashState: string;
  lastEventTs: number;
  liveness: string;
  /** Newest shell "Recent" timestamp, 0 when the file was never opened. */
  openedTs: number;
}

export interface EventRow {
  id: number;
  kind: string;
  ts: number;
  size: number;
  processName: string | null;
  processPath: string | null;
  pid: number | null;
  attribution: string;
  note: string | null;
}

export interface CopyRow {
  id: number;
  path: string;
  size: number;
  mtime: number;
  /** Same `(volume, file index)`: a hard link, i.e. the very same data. */
  hardlink: boolean;
}

/** A file that shares some 1 MiB chunk digests — "same project", not a copy. */
export interface RelatedRow {
  id: number;
  path: string;
  size: number;
  sharedChunks: number;
  totalChunks: number;
}

export interface GraphNode {
  id: string;
  label: string;
  kind: string;
  sub: string | null;
  weight: number;
}

export interface GraphEdge {
  from: string;
  to: string;
  label: string;
  kind: string;
}

export interface Graph {
  nodes: GraphNode[];
  edges: GraphEdge[];
}

export interface Answer {
  headline: string;
  detail: string;
  /** high | medium | low */
  confidence: string;
}

export interface Answers {
  origin: Answer;
  modifiers: Answer;
  safety: Answer;
}

/** Q1+Q2+Q3 folded into one conclusion with a derived confidence. */
export interface Verdict {
  headline: string;
  detail: string;
  /** high | medium | low */
  confidence: string;
  /** 5..=97, computed from evidence strength on the Rust side. */
  score: number;
  drivers: string[];
  caveats: string[];
  /** delete_safe | keep | verify_first */
  action: string;
}

export interface FileDetail {
  file: FileRow;
  versions: EventRow[];
  copies: CopyRow[];
  related: RelatedRow[];
  graph: Graph;
  answers: Answers;
  verdict: Verdict;
}

export interface Status {
  watching: boolean;
  dirs: string[];
  fileCount: number;
  eventCount: number;
  hashedCount: number;
  pendingHash: number;
  scanState: string;
  scanTotal: number;
  scanDone: number;
  eventsThisSession: number;
  attributionHits: number;
  attributionMisses: number;
  dbPath: string;
  dbSize: number;
  startedAt: number;
  lastEventAt: number;
  lastError: string | null;
  adapters: string[];
  sourceRecords: number;

  // ---- collection hardening ------------------------------------------------
  elevated: boolean;
  /** `disabled` | `active` | `denied: …` | `unsupported: …` | `error: …` */
  etwState: string;
  etwEvents: number;
  /** Registry/plist/desktop-file truth. */
  autostart: boolean;
  /** What `config.ini` says — may differ from `autostart` when a write failed. */
  autostartConfigured: boolean;
  autostartCommand: string | null;
  configPath: string;
  archivedEvents: number;
  archiveFiles: number;
  lastArchiveAt: number;
  identifiedFiles: number;
  chunkCount: number;
  incrementalHashes: number;
  recentEntries: number;
  totalDbSize: number;
  schemaVersion: number;

  // ---- 0.4.0: full-disk index + fingerprint scope --------------------------
  /** idle | indexing | done — independent of the hash queue. */
  indexState: string;
  /** Drive roots / Unix root actually walked by the indexer. */
  indexRoots: string[];
  /** Directories with fingerprints + real-time events ("hot dirs"). */
  hotDirs: string[];
  /** True when FTS5 trigram is available and queries use MATCH. */
  ftsEnabled: boolean;
  /** The last index pass reached `max_scan_files`; kept in the ready state. */
  scanTruncated: boolean;
  /** Honest description of what real-time watching actually covers. */
  watchScope: string;
}

export const emptyStatus: Status = {
  watching: false,
  dirs: [],
  fileCount: 0,
  eventCount: 0,
  hashedCount: 0,
  pendingHash: 0,
  scanState: "idle",
  scanTotal: 0,
  scanDone: 0,
  eventsThisSession: 0,
  attributionHits: 0,
  attributionMisses: 0,
  dbPath: "",
  dbSize: 0,
  startedAt: 0,
  lastEventAt: 0,
  lastError: null,
  adapters: [],
  sourceRecords: 0,
  elevated: false,
  etwState: "starting",
  etwEvents: 0,
  autostart: false,
  autostartConfigured: false,
  autostartCommand: null,
  configPath: "",
  archivedEvents: 0,
  archiveFiles: 0,
  lastArchiveAt: 0,
  identifiedFiles: 0,
  chunkCount: 0,
  incrementalHashes: 0,
  recentEntries: 0,
  totalDbSize: 0,
  schemaVersion: 0,
  indexState: "idle",
  indexRoots: [],
  hotDirs: [],
  ftsEnabled: false,
  scanTruncated: false,
  watchScope: "",
};
