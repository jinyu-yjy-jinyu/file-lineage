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
  return path.replace(/^C:\\Users\\[^\\]+\\/i, "~\\");
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
};
