import { invoke } from "@tauri-apps/api/core";

import type { FileDetail, FileRow, Status } from "./types";

/** True when running inside the Tauri shell (vs. `vite dev` in a browser). */
export const isTauri = (): boolean =>
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

const now = Date.now();

const demoFiles: FileRow[] = [
  {
    id: 1,
    path: "C:\\Users\\Demo\\Downloads\\Q3-财报-final-v3.xlsx",
    name: "Q3-财报-final-v3.xlsx",
    ext: "xlsx",
    size: 2_413_568,
    mtime: now - 1000 * 60 * 42,
    sourceKind: "download",
    sourceLabel: "Edge 下载 · mail.qq.com",
    sourceDetail:
      "来源页面：mail.qq.com\n下载时间：2026-03-01 04:12:00 UTC\n证据：浏览器下载记录 (只读快照)",
    sourceEvidence: "browser.exact",
    sourceStrength: 3,
    changeCount: 4,
    createdAt: now - 1000 * 60 * 60 * 26,
    duplicateCount: 2,
    contentHash: "a1b2c3d4e5f60718",
    hashState: "done",
    lastEventTs: now - 1000 * 60 * 42,
    liveness: "live",
    openedTs: now - 1000 * 60 * 55,
  },
  {
    id: 2,
    path: "C:\\Users\\Demo\\Documents\\合同扫描件.pdf",
    name: "合同扫描件.pdf",
    ext: "pdf",
    size: 18_874_112,
    mtime: now - 1000 * 60 * 60 * 5,
    sourceKind: "im",
    sourceLabel: "微信 4.x（账号 wxid_demo，接收于 2026-02）",
    sourceDetail: "路径特征：微信 FileStorage 接收目录",
    sourceEvidence: "im.layout",
    sourceStrength: 2,
    changeCount: 1,
    createdAt: now - 1000 * 60 * 60 * 8,
    duplicateCount: 0,
    contentHash: "9988aa77bb66cc55",
    hashState: "done",
    lastEventTs: now - 1000 * 60 * 60 * 5,
    liveness: "live",
    openedTs: 0,
  },
];

const demoDetail: FileDetail = {
  file: demoFiles[0],
  versions: [
    {
      id: 4,
      kind: "opened",
      ts: now - 1000 * 60 * 55,
      size: 0,
      processName: null,
      processPath: null,
      pid: null,
      attribution: "recent-lnk",
      note: "Shell「最近使用」记录（1 个快捷方式）。打开者不会出现在文件写入事件里，因此这是推断信号。",
    },
    {
      id: 3,
      kind: "modified",
      ts: now - 1000 * 60 * 42,
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
      ts: now - 1000 * 60 * 60 * 3,
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
      ts: now - 1000 * 60 * 60 * 26,
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
      mtime: now - 1000 * 60 * 30,
      hardlink: false,
    },
  ],
  related: [
    {
      id: 11,
      path: "C:\\Users\\Demo\\Documents\\Q3-财报-模板.xlsx",
      size: 2_388_224,
      sharedChunks: 2,
      totalChunks: 3,
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
      detail:
        "来源页面：mail.qq.com\n证据：浏览器下载记录（精确路径命中）\n证据通道：browser.exact（强度 3）。",
      confidence: "high",
    },
    modifiers: {
      headline: "2 个进程碰过它",
      detail:
        "活进程写入者（按事件次数排序）：EXCEL.EXE（2 次）。\n主要证据来自Restart Manager 写句柄（只证明写入者）。\n统计口径：全库共 4 条事件；时间线只展示最近 4 条。",
      confidence: "medium",
    },
    safety: {
      headline: "还有 1 个副本，删除不会丢内容",
      detail:
        "BLAKE3 指纹 a1b2c3d4e5f60718…：\n内容完全相同的独立副本 1 个：\n  · C:\\Users\\Demo\\Desktop\\Q3-财报-final-v3 (1).xlsx",
      confidence: "high",
    },
  },
  verdict: {
    headline: "删除不会丢内容：另有 1 个相同内容的副本",
    detail:
      "内容指纹在其它位置命中同一份数据，删除此路径不会丢失内容。（可信度 78 / 100：证据可用但存在缺口，建议结合保留意见判断）",
    confidence: "medium",
    score: 78,
    drivers: [
      "来源证据：浏览器下载记录（精确路径命中） · 客观权威（strength=3）",
      "进程归因：Restart Manager 写句柄，1 个写入者",
      "内容指纹已完成：存在 1 个独立副本",
      "另有 1 个文件共享部分内容块（关联文件，不计入副本）",
    ],
    caveats: [
      "ETW 内核事件流未启用（未提权或被配置关闭），写入者归因只有句柄近似",
      "部分「被打开」记录按文件名匹配，属于推断信号而非硬证据",
    ],
    action: "delete_safe",
  },
};

/** Demo mode mirrors the backend's server-side kind filter. */
function demoFiltered(query: string, kind: string): FileRow[] {
  const q = query.trim().toLowerCase();
  return demoFiles.filter(
    (f) =>
      (q === "" || f.name.toLowerCase().includes(q) || f.path.toLowerCase().includes(q)) &&
      (kind === "" || f.sourceKind === kind),
  );
}

export async function getStatus(): Promise<Status> {
  if (!isTauri()) {
    return {
      watching: true,
      dirs: ["C:\\Users\\Demo\\Downloads", "C:\\Users\\Demo\\Desktop", "C:\\Users\\Demo\\Documents"],
      fileCount: demoFiles.length,
      eventCount: 4,
      hashedCount: 2,
      pendingHash: 0,
      scanState: "ready",
      scanTotal: 2,
      scanDone: 2,
      eventsThisSession: 1,
      attributionHits: 2,
      attributionMisses: 2,
      dbPath: "C:\\Users\\Demo\\AppData\\Local\\FileLineage\\lineage.db",
      dbSize: 147_456,
      startedAt: now - 1000 * 60 * 12,
      lastEventAt: now - 1000 * 60 * 42,
      lastError: null,
      adapters: [
        "Zone.Identifier 来源流（与浏览器无关的下载证据）",
        "Shell「最近使用」打开记录（12 条）",
        "Edge (12 条下载记录)",
        "Chrome (8 条下载记录)",
        "IM 接收目录布局（账号 + 接收月份）",
      ],
      sourceRecords: 20,
      elevated: false,
      etwState: "denied: 需要管理员权限，当前使用 Restart Manager",
      etwEvents: 0,
      autostart: true,
      autostartConfigured: true,
      autostartCommand: '"C:\\Program Files\\FileLineage\\FileLineage.exe" --minimized',
      configPath: "C:\\Users\\Demo\\AppData\\Local\\FileLineage\\config.ini",
      archivedEvents: 128,
      archiveFiles: 2,
      lastArchiveAt: now - 1000 * 60 * 60 * 20,
      identifiedFiles: 2,
      chunkCount: 6,
      incrementalHashes: 1,
      recentEntries: 12,
      totalDbSize: 393_216,
      schemaVersion: 3,
    };
  }
  return invoke<Status>("get_status");
}

export async function listFiles(
  query: string,
  kind = "",
  limit = 300,
  offset = 0,
): Promise<FileRow[]> {
  if (!isTauri()) {
    return demoFiltered(query, kind).slice(offset, offset + limit);
  }
  return invoke<FileRow[]>("list_files", { query, kind, limit, offset });
}

export async function countFiles(query: string, kind = ""): Promise<number> {
  if (!isTauri()) {
    return demoFiltered(query, kind).length;
  }
  return invoke<number>("count_files", { query, kind });
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
