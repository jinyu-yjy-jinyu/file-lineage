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
  changeCount: number;
  createdAt: number;
  duplicateCount: number;
  contentHash: string | null;
  hashState: string;
  lastEventTs: number;
  liveness: string;
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
  confidence: string;
}

export interface Answers {
  origin: Answer;
  modifiers: Answer;
  safety: Answer;
}

export interface FileDetail {
  file: FileRow;
  versions: EventRow[];
  copies: CopyRow[];
  graph: Graph;
  answers: Answers;
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
};
