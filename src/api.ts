import { invoke } from "@tauri-apps/api/core";

import type { FileDetail, FileRow, Status } from "./types";

/** True when running inside the Tauri shell (vs. `vite dev` in a browser). */
export const isTauri = (): boolean =>
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

const demoFiles: FileRow[] = [
  {
    id: 1,
    path: "C:\\Users\\Demo\\Downloads\\Q3-财报-final-v3.xlsx",
    name: "Q3-财报-final-v3.xlsx",
    ext: "xlsx",
    size: 2_413_568,
    mtime: Date.now() - 1000 * 60 * 42,
    sourceKind: "download",
    sourceLabel: "Edge 下载 · mail.qq.com",
    sourceDetail: "证据：浏览器下载记录（只读快照）",
    changeCount: 4,
    createdAt: Date.now() - 1000 * 60 * 60 * 26,
    duplicateCount: 2,
    contentHash: "a1b2c3d4e5f60718",
    hashState: "done",
    lastEventTs: Date.now() - 1000 * 60 * 42,
    liveness: "live",
  },
  {
    id: 2,
    path: "C:\\Users\\Demo\\Documents\\合同扫描件.pdf",
    name: "合同扫描件.pdf",
    ext: "pdf",
    size: 18_874_112,
    mtime: Date.now() - 1000 * 60 * 60 * 5,
    sourceKind: "im",
    sourceLabel: "微信接收文件",
    sourceDetail: "路径特征：微信 FileStorage 接收目录",
    changeCount: 1,
    createdAt: Date.now() - 1000 * 60 * 60 * 8,
    duplicateCount: 0,
    contentHash: "9988aa77bb66cc55",
    hashState: "done",
    lastEventTs: Date.now() - 1000 * 60 * 60 * 5,
    liveness: "live",
  },
];

const demoDetail: FileDetail = {
  file: demoFiles[0],
  versions: [
    {
      id: 3,
      kind: "modified",
      ts: Date.now() - 1000 * 60 * 42,
      size: 2_413_568,
      processName: "EXCEL.EXE",
      processPath: "C:\\Program Files\\Microsoft Office\\EXCEL.EXE",
      pid: 8812,
      attribution: "restart-manager",
      note: null,
    },
    {
      id: 2,
      kind: "modified",
      ts: Date.now() - 1000 * 60 * 60 * 3,
      size: 2_401_792,
      processName: "EXCEL.EXE",
      processPath: null,
      pid: 7340,
      attribution: "restart-manager",
      note: null,
    },
    {
      id: 1,
      kind: "observed",
      ts: Date.now() - 1000 * 60 * 60 * 26,
      size: 2_388_224,
      processName: null,
      processPath: null,
      pid: null,
      attribution: "baseline",
      note: "首次扫描登记：只代表我们发现了这个文件，不代表它的创建时间",
    },
  ],
  copies: [
    {
      id: 7,
      path: "C:\\Users\\Demo\\Desktop\\Q3-财报-final-v3 (1).xlsx",
      size: 2_413_568,
      mtime: Date.now() - 1000 * 60 * 30,
    },
  ],
  graph: {
    nodes: [
      { id: "source:1", label: "Edge 下载 · mail.qq.com", kind: "source", sub: "download", weight: 1.2 },
      { id: "file:1", label: "Q3-财报-final-v3.xlsx", kind: "file", sub: "2.3 MB", weight: 2.2 },
      { id: "ver:1", label: "v1", kind: "version", sub: "首次登记", weight: 1 },
      { id: "ver:2", label: "v2", kind: "version", sub: "修改", weight: 1 },
      { id: "ver:3", label: "v3", kind: "version", sub: "修改", weight: 1 },
      { id: "proc:excel.exe", label: "EXCEL.EXE", kind: "process", sub: "持有写句柄", weight: 1.1 },
      { id: "copy:7", label: "Q3-财报-final-v3 (1).xlsx", kind: "copy", sub: "桌面", weight: 0.9 },
    ],
    edges: [
      { from: "source:1", to: "file:1", label: "来源于", kind: "origin" },
      { from: "ver:1", to: "ver:2", label: "演进", kind: "evolve" },
      { from: "ver:2", to: "ver:3", label: "演进", kind: "evolve" },
      { from: "ver:3", to: "file:1", label: "当前版本", kind: "evolve" },
      { from: "proc:excel.exe", to: "ver:2", label: "写入", kind: "touch" },
      { from: "proc:excel.exe", to: "ver:3", label: "写入", kind: "touch" },
      { from: "file:1", to: "copy:7", label: "内容相同", kind: "copy" },
    ],
  },
  answers: {
    origin: {
      headline: "来自外部下载",
      detail: "来源页面：mail.qq.com\n证据：浏览器下载记录（只读快照）",
      confidence: "high",
    },
    modifiers: {
      headline: "1 个进程碰过它",
      detail: "按事件次数排序：EXCEL.EXE（2 次）。证据来自 Windows Restart Manager 的句柄归属。",
      confidence: "medium",
    },
    safety: {
      headline: "还有 1 个副本，删除不会丢内容",
      detail: "BLAKE3 指纹 a1b2c3d4e5f60718… 在桌面存在相同内容。",
      confidence: "high",
    },
  },
};

export async function getStatus(): Promise<Status> {
  if (!isTauri()) {
    return {
      watching: true,
      dirs: ["C:\\Users\\Demo\\Downloads", "C:\\Users\\Demo\\Desktop", "C:\\Users\\Demo\\Documents"],
      fileCount: demoFiles.length,
      eventCount: 3,
      hashedCount: 2,
      pendingHash: 0,
      scanState: "ready",
      scanTotal: 2,
      scanDone: 2,
      eventsThisSession: 1,
      attributionHits: 2,
      attributionMisses: 1,
      dbPath: "C:\\Users\\Demo\\AppData\\Local\\FileLineage\\lineage.db",
      dbSize: 131_072,
      startedAt: Date.now() - 1000 * 60 * 12,
      lastEventAt: Date.now() - 1000 * 60 * 42,
      lastError: null,
      adapters: ["Edge (12 条下载记录)", "Chrome (8 条下载记录)"],
      sourceRecords: 20,
    };
  }
  return invoke<Status>("get_status");
}

export async function listFiles(query: string, limit = 300, offset = 0): Promise<FileRow[]> {
  if (!isTauri()) {
    return demoFiles.filter((f) => f.name.toLowerCase().includes(query.toLowerCase()));
  }
  return invoke<FileRow[]>("list_files", { query, limit, offset });
}

export async function getFileDetail(id: number): Promise<FileDetail | null> {
  if (!isTauri()) {
    return id === demoDetail.file.id ? demoDetail : null;
  }
  return invoke<FileDetail | null>("get_file_detail", { id });
}

export async function rescan(): Promise<void> {
  if (!isTauri()) return;
  await invoke<void>("rescan");
}

export async function reveal(path: string): Promise<void> {
  if (!isTauri()) return;
  await invoke<void>("reveal", { path });
}
