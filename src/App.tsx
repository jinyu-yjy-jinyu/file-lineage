import { useCallback, useEffect, useMemo, useState } from "react";

import * as api from "./api";
import { LineageGraph } from "./components/LineageGraph";
import {
  basename,
  eventLabel,
  formatBytes,
  formatTime,
  relativeTime,
  shortPath,
  sourceMeta,
} from "./format";
import type { Answer, FileDetail, FileRow, Status } from "./types";
import { emptyStatus } from "./types";

const FILTERS = [
  { key: "all", label: "全部" },
  { key: "download", label: "下载" },
  { key: "email", label: "邮件" },
  { key: "im", label: "IM" },
  { key: "local", label: "本地" },
  { key: "unknown", label: "未知" },
];

function SourceBadge({ kind }: { kind: string }) {
  const meta = sourceMeta[kind] ?? sourceMeta.unknown;
  return <span className={`badge ${meta.className}`}>{meta.label}</span>;
}

function AnswerCard({
  index,
  question,
  answer,
  tone,
}: {
  index: string;
  question: string;
  answer: Answer;
  tone: string;
}) {
  const conf =
    answer.confidence === "high" ? "高置信" : answer.confidence === "medium" ? "中置信" : "低置信";
  return (
    <section className={`answer-card ${tone}`}>
      <div className="answer-top">
        <span className="answer-index">{index}</span>
        <span className="answer-question">{question}</span>
        <span className={`confidence conf-${answer.confidence}`}>{conf}</span>
      </div>
      <h3>{answer.headline}</h3>
      <p>{answer.detail}</p>
    </section>
  );
}

function Stat({ label, value, hint }: { label: string; value: string; hint?: string }) {
  return (
    <div className="stat">
      <span className="stat-label">{label}</span>
      <span className="stat-value">{value}</span>
      {hint ? <span className="stat-hint">{hint}</span> : null}
    </div>
  );
}

export default function App() {
  const [status, setStatus] = useState<Status>(emptyStatus);
  const [files, setFiles] = useState<FileRow[]>([]);
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState("all");
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [detail, setDetail] = useState<FileDetail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [rescanning, setRescanning] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const [nextStatus, nextFiles] = await Promise.all([
        api.getStatus(),
        api.listFiles(query),
      ]);
      setStatus(nextStatus);
      setFiles(nextFiles);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, [query]);

  const loadDetail = useCallback(async (id: number | null) => {
    if (id === null) {
      setDetail(null);
      return;
    }
    try {
      setDetail(await api.getFileDetail(id));
    } catch {
      setDetail(null);
    }
  }, []);

  useEffect(() => {
    void refresh();
    void loadDetail(selectedId);
    const timer = window.setInterval(() => {
      void refresh();
      void loadDetail(selectedId);
    }, 2_500);
    return () => window.clearInterval(timer);
  }, [refresh, loadDetail, selectedId]);

  useEffect(() => {
    if (selectedId === null && files.length > 0) setSelectedId(files[0].id);
  }, [files, selectedId]);

  const visibleFiles = useMemo(
    () => (filter === "all" ? files : files.filter((f) => f.sourceKind === filter)),
    [files, filter],
  );

  const onRescan = async () => {
    setRescanning(true);
    try {
      await api.rescan();
      window.setTimeout(() => void refresh(), 600);
    } finally {
      window.setTimeout(() => setRescanning(false), 900);
    }
  };

  const scanLabel =
    status.scanState === "scanning"
      ? `扫描中 ${status.scanDone}/${status.scanTotal || "…"}`
      : status.scanState === "hashing"
        ? `指纹计算中 · 待处理 ${status.pendingHash}`
        : status.scanState === "ready"
          ? "采集就绪"
          : "待机";

  return (
    <div className="app">
      <header className="topbar">
        <div className="brand">
          <div className="brand-mark">
            <span />
            <span />
            <span />
          </div>
          <div>
            <h1>文件因果溯源台</h1>
            <p>FILELINEAGE · V1 · Windows</p>
          </div>
        </div>
        <div className="topbar-center">
          <span className={`live-dot ${status.watching ? "on" : "off"}`} />
          <span className="live-text">
            {status.watching ? "后台采集中" : "监控未启动"}
          </span>
          <span className="divider" />
          <span className="scan-pill">{scanLabel}</span>
          {status.pendingHash > 0 ? (
            <span className="pending-pill">指纹队列 {status.pendingHash}</span>
          ) : null}
        </div>
        <div className="topbar-actions">
          <span className="version-tag">v0.2.2</span>
          <button className="btn" onClick={() => void onRescan()} disabled={rescanning}>
            {rescanning ? "重新扫描中…" : "重新扫描"}
          </button>
        </div>
      </header>

      {error ? <div className="error-bar">后端通信异常：{error}</div> : null}
      {status.lastError ? <div className="error-bar">采集告警：{status.lastError}</div> : null}

      <div className="layout">
        <aside className="sidebar">
          <div className="side-section">
            <h2>监控目录</h2>
            <ul className="dir-list">
              {status.dirs.length === 0 ? <li className="muted">正在探测…</li> : null}
              {status.dirs.map((d) => (
                <li key={d} title={d}>
                  <span className="dir-icon">▤</span>
                  <span className="dir-name">{basename(d)}</span>
                </li>
              ))}
            </ul>
          </div>

          <div className="side-section">
            <h2>采集概况</h2>
            <div className="stat-grid">
              <Stat label="文件" value={status.fileCount.toLocaleString()} />
              <Stat label="事件" value={status.eventCount.toLocaleString()} />
              <Stat label="已完成指纹" value={status.hashedCount.toLocaleString()} />
              <Stat
                label="本次会话事件"
                value={status.eventsThisSession.toLocaleString()}
                hint={status.lastEventAt ? relativeTime(status.lastEventAt) : "暂无"}
              />
            </div>
            <div className="attrib-row">
              <span>进程归因</span>
              <div className="attrib-bar">
                <div
                  style={{
                    width: `${(() => {
                      const total = status.attributionHits + status.attributionMisses;
                      return total === 0 ? 0 : (status.attributionHits / total) * 100;
                    })()}%`,
                  }}
                />
              </div>
              <span className="muted">{status.attributionHits}/{status.attributionHits + status.attributionMisses}</span>
            </div>
          </div>

          <div className="side-section">
            <h2>来源适配器</h2>
            {status.adapters.length === 0 ? (
              <p className="muted small">尚未加载；点击「重新扫描」刷新。</p>
            ) : (
              <ul className="adapter-list">
                {status.adapters.map((a) => (
                  <li key={a}>{a}</li>
                ))}
              </ul>
            )}
            <p className="muted small">共 {status.sourceRecords} 条下载记录 · 只读快照</p>
          </div>

          <div className="side-section side-bottom">
            <h2>本地数据库</h2>
            <p className="mono small break">{status.dbPath || "—"}</p>
            <p className="muted small">大小 {formatBytes(status.dbSize)} · SQLite WAL</p>
            <div className="v1-note">
              <strong>监控范围 · V2.0.2</strong>
              <p>
                递归监控 下载 / 桌面 / 文档 三个目录（含子目录），回答「从哪来 / 谁改过 / 删了会不会丢」。
              </p>
              <p>
                来源证据：下载器任务记录、Zone.Identifier、浏览器下载库、IM 接收目录、Shell 打开记录。
                写入事件与进程归因从安装后开始积累。
              </p>
            </div>
          </div>
        </aside>

        <main className="center">
          <div className="toolbar">
            <div className="search">
              <span className="search-icon">⌕</span>
              <input
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder="按文件名或路径筛选…"
                spellCheck={false}
              />
            </div>
            <span className="result-count">{visibleFiles.length} 个文件</span>
          </div>
          <div className="filters">
            {FILTERS.map((f) => (
              <button
                key={f.key}
                className={`chip ${filter === f.key ? "active" : ""}`}
                onClick={() => setFilter(f.key)}
              >
                {f.label}
              </button>
            ))}
          </div>

          <div className="table-wrap">
            <table className="file-table">
              <thead>
                <tr>
                  <th>文件</th>
                  <th>来源</th>
                  <th className="num">大小</th>
                  <th className="num">变更</th>
                  <th className="num">副本</th>
                  <th>最近活动</th>
                </tr>
              </thead>
              <tbody>
                {visibleFiles.map((f) => (
                  <tr
                    key={f.id}
                    className={`${selectedId === f.id ? "selected" : ""} ${f.liveness === "gone" ? "gone" : ""}`}
                    onClick={() => setSelectedId(f.id)}
                  >
                    <td>
                      <div className="file-cell">
                        <span className="ext-dot">{f.ext.slice(0, 3).toUpperCase() || "···"}</span>
                        <div>
                          <div className="file-name" title={f.path}>{f.name}</div>
                          <div className="file-path" title={f.path}>{shortPath(f.path)}</div>
                        </div>
                      </div>
                    </td>
                    <td><SourceBadge kind={f.sourceKind} /></td>
                    <td className="num mono">{formatBytes(f.size)}</td>
                    <td className="num mono">{f.changeCount}</td>
                    <td className="num">
                      {f.duplicateCount > 0 ? (
                        <span className="dup-yes">+{f.duplicateCount}</span>
                      ) : (
                        <span className="muted">—</span>
                      )}
                    </td>
                    <td className="muted nowrap">{relativeTime(f.lastEventTs)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
            {visibleFiles.length === 0 ? (
              <div className="empty">
                <div className="empty-icon">◇</div>
                <p>{status.scanState === "scanning" ? "正在建立目录基线…" : "没有匹配的文件"}</p>
                <span>首次启动需要扫描三个目录，稍等片刻即可看到结果。</span>
              </div>
            ) : null}
          </div>
        </main>

        <aside className="detail">
          {!detail ? (
            <div className="detail-empty">
              <div className="empty-icon">◎</div>
              <p>选择一个文件，查看它的一生</p>
              <span>从哪来 · 经过谁的手 · 还有没有用</span>
            </div>
          ) : (
            <div className="detail-scroll">
              <div className="detail-head">
                <div className="detail-title">{detail.file.name}</div>
                <div className="detail-sub mono break">{detail.file.path}</div>
                <div className="detail-meta">
                  <SourceBadge kind={detail.file.sourceKind} />
                  <span className="mono muted">{formatBytes(detail.file.size)}</span>
                  <span className="mono muted">BLAKE3 {detail.file.contentHash ? detail.file.contentHash.slice(0, 12) + "…" : "计算中"}</span>
                  <button className="btn tiny" onClick={() => void api.reveal(detail.file.path)}>
                    在资源管理器中显示
                  </button>
                </div>
              </div>

              <AnswerCard index="Q1" question="它从哪来？" answer={detail.answers.origin} tone="tone-origin" />
              <AnswerCard index="Q2" question="谁改过它？" answer={detail.answers.modifiers} tone="tone-process" />
              <AnswerCard index="Q3" question="删了会丢东西吗？" answer={detail.answers.safety} tone="tone-safety" />

              <section className="panel">
                <div className="panel-head">
                  <h2>血缘视图</h2>
                  <span className="muted small">{detail.graph.nodes.length} 节点 / {detail.graph.edges.length} 边</span>
                </div>
                <LineageGraph graph={detail.graph} />
                <div className="legend">
                  <span><i style={{ background: "#ffb25c" }} />来源</span>
                  <span><i style={{ background: "#56d6c8" }} />文件</span>
                  <span><i style={{ background: "#7a9cc0" }} />版本</span>
                  <span><i style={{ background: "#9f8cff" }} />进程</span>
                  <span><i style={{ background: "#6fcf97" }} />副本</span>
                </div>
              </section>

              <section className="panel">
                <div className="panel-head"><h2>变更时间线</h2><span className="muted small">最近 {detail.versions.length} 条</span></div>
                <ol className="timeline">
                  {detail.versions.length === 0 ? <li className="muted small">暂无事件</li> : null}
                  {detail.versions.map((ev) => (
                    <li key={ev.id}>
                      <span className="timeline-dot" />
                      <div className="timeline-body">
                        <div className="timeline-top">
                          <strong>{eventLabel[ev.kind] ?? ev.kind}</strong>
                          <span className="mono muted small">{formatTime(ev.ts)}</span>
                        </div>
                        <div className="timeline-detail">
                          {ev.processName ? (
                            <span className="proc-tag">{ev.processName}{ev.pid ? ` · PID ${ev.pid}` : ""}</span>
                          ) : (
                            <span className="muted small">无进程归因</span>
                          )}
                          <span className="mono muted small">{formatBytes(ev.size)}</span>
                        </div>
                        {ev.note ? <div className="muted small">{ev.note}</div> : null}
                      </div>
                    </li>
                  ))}
                </ol>
              </section>

              {detail.copies.length > 0 ? (
                <section className="panel">
                  <div className="panel-head"><h2>相同内容的其他位置</h2></div>
                  <ul className="copy-list">
                    {detail.copies.map((c) => (
                      <li key={c.id} className="mono small break">{c.path}</li>
                    ))}
                  </ul>
                </section>
              ) : null}
            </div>
          )}
        </aside>
      </div>
    </div>
  );
}
