export function formatBytes(n: number): string {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let v = Math.max(0, n);
  let i = 0;
  while (v >= 1024 && i + 1 < units.length) {
    v /= 1024;
    i += 1;
  }
  return i === 0 ? `${n} B` : `${v.toFixed(1)} ${units[i]}`;
}

/**
 * Local wall-clock time. The backend never hand-formats timestamps in prose
 * (it appends an explicit `UTC` label where it must, e.g. browser download
 * times), so a single event can no longer appear as two different moments.
 */
export function formatTime(ms: number): string {
  if (!ms || ms <= 0) return "—";
  const d = new Date(ms);
  const pad = (x: number) => String(x).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

export function relativeTime(ms: number): string {
  if (!ms || ms <= 0) return "—";
  const diff = Date.now() - ms;
  if (diff < 0) return formatTime(ms);
  const mins = Math.floor(diff / 60_000);
  if (mins < 1) return "刚刚";
  if (mins < 60) return `${mins} 分钟前`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours} 小时前`;
  const days = Math.floor(hours / 24);
  if (days < 30) return `${days} 天前`;
  return formatTime(ms);
}

export function basename(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

export function shortPath(path: string): string {
  return path.replace(/^[A-Z]:\\Users\\[^\\]+\\/i, "~\\");
}

/** UTF-16-safe truncation: `slice` can split a surrogate pair (L17). */
export function truncateChars(text: string, max: number): string {
  const chars = Array.from(text);
  return chars.length > max ? `${chars.slice(0, max - 1).join("")}…` : text;
}

export function confidenceLabel(confidence: string): string {
  if (confidence === "high") return "高置信";
  if (confidence === "medium") return "中置信";
  return "低置信";
}

export const sourceMeta: Record<string, { label: string; className: string }> = {
  download: { label: "下载", className: "badge-download" },
  email: { label: "邮件", className: "badge-email" },
  im: { label: "IM", className: "badge-im" },
  local: { label: "本地", className: "badge-local" },
  unknown: { label: "未知", className: "badge-unknown" },
};

export const eventLabel: Record<string, string> = {
  created: "创建",
  modified: "修改",
  renamed: "重命名",
  removed: "删除",
  observed: "首次登记",
  opened: "被打开",
  downloaded: "下载",
};

/** How an event got its process, rendered as a small badge in the timeline. */
export const attributionLabel: Record<string, string> = {
  etw: "ETW 写入",
  "restart-manager": "写句柄",
  "download-task": "下载器记录",
  "recent-lnk": "推断",
  baseline: "基线",
  unknown: "无归因",
};

export function isInferredAttribution(attribution: string): boolean {
  return attribution === "recent-lnk";
}
