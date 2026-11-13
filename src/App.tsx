import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import * as api from "./api";
import { LineageGraph } from "./components/LineageGraph";
import {
  attributionLabel,
  basename,
  confidenceLabel,
  eventLabel,
  formatBytes,
  formatTime,
  isInferredAttribution,
  relativeTime,
  shortPath,
  sourceMeta,
  truncateChars,
} from "./format";
import { confidenceGlyphs, graphLegend } from "./theme";
import type { Answer, FileDetail, FileRow, Status, Verdict } from "./types";
import { emptyStatus } from "./types";

const APP_VERSION = __APP_VERSION__;

const FILTERS = [
  { key: "all", label: "全部" },
  { key: "download", label: "下载" },
  { key: "email", label: "邮件" },
  { key: "im", label: "IM" },
  { key: "local", label: "本地" },
  { key: "unknown", label: "未知" },
];

const PAGE_SIZE = 100;

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
  return (
    <section className={`answer-card ${tone}`}>
      <div className="answer-top">
        <span className="answer-index">{index}</span>
        <span className="answer-question">{question}</span>
        <span className={`confidence conf-${answer.confidence}`}>
          <span className="conf-dots" aria-hidden="true">
            {confidenceGlyphs(answer.confidence)}
          </span>
          {confidenceLabel(answer.confidence)}
        </span>
      </div>
      <h3>{answer.headline}</h3>
      <p>{answer.detail}</p>
    </section>
  );
}

const ACTION_LABEL: Record<string, string> = {
  delete_safe: "可以删除",
  keep: "建议保留",
  verify_first: "先核实再决定",
};

function VerdictCard({ verdict }: { verdict: Verdict }) {
  // The score is derived on the Rust side from the evidence channels; the bar
  // is only a visualisation of it (5..=97 → 0..100%).
  const percent = Math.round(((verdict.score - 5) / 92) * 100);
  return (
    <section className={`verdict-card verdict-${verdict.confidence}`}>
      <div className="verdict-top">
        <span className="verdict-tag">最终结论</span>
        <span className="verdict-action">{ACTION_LABEL[verdict.action] ?? verdict.action}</span>
        <span className={`confidence conf-${verdict.confidence}`}>
          <span className="conf-dots" aria-hidden="true">
            {confidenceGlyphs(verdict.confidence)}
          </span>
          {confidenceLabel(verdict.confidence)}
        </span>
      </div>
      <h3>{verdict.headline}</h3>
      <p className="verdict-detail">{verdict.detail}</p>
      <div className="verdict-meter">
        <div className="verdict-meter-track">
          <div className="verdict-meter-fill" style={{ width: `${percent}%` }} />
        </div>
        <span className="mono muted">可信度 {verdict.score}/100</span>
      </div>
      <div className="verdict-grid">
        <div>
          <h4>依据</h4>
          <ul>
            {verdict.drivers.map((item) => (
              <li key={item}>{item}</li>
            ))}
          </ul>
        </div>
        {verdict.caveats.length > 0 ? (
          <div>
            <h4>保留意见</h4>
            <ul>
              {verdict.caveats.map((item) => (
                <li key={item}>{item}</li>
              ))}
            </ul>
          </div>
        ) : null}
      </div>
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
  const [total, setTotal] = useState(0);
  const [query, setQuery] = useState("");
  const [debouncedQuery, setDebouncedQuery] = useState("");
  const [filter, setFilter] = useState("all");
  const [page, setPage] = useState(0);
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [detail, setDetail] = useState<FileDetail | null>(null);
  const [detailTick, setDetailTick] = useState(0);
  const [error, setError] = useState<string | null>(null);

  const kind = filter === "all" ? "" : filter;
  const pageCount = Math.max(1, Math.ceil(total / PAGE_SIZE));

  // ---- query debounce (M9) ------------------------------------------------
  useEffect(() => {
    const timer = window.setTimeout(() => {
      setDebouncedQuery(query);
      setPage(0);
    }, 300);
    return () => window.clearTimeout(timer);
  }, [query]);

  // ---- status poll: 5 s, no dependency on the query ------------------------
  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      try {
        const next = await api.getStatus();
        if (!cancelled) {
          setStatus(next);
          setError(null);
        }
      } catch (e) {
        if (!cancelled) setError(String(e));
      }
    };
    void load();
    const timer = window.setInterval(() => void load(), 5_000);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, []);

  // ---- server-side filtering + pagination (M3) -----------------------------
  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      try {
        const [rows, count] = await Promise.all([
          api.listFiles(debouncedQuery, kind, PAGE_SIZE, page * PAGE_SIZE),
          api.countFiles(debouncedQuery, kind),
        ]);
        if (cancelled) return;
        setFiles(rows);
        setTotal(count);
      } catch (e) {
        if (!cancelled) setError(String(e));
      }
    };
    void load();
    return () => {
      cancelled = true;
    };
  }, [debouncedQuery, kind, page]);

  // ---- detail: on demand, not on every poll -------------------------------
  useEffect(() => {
    let cancelled = false;
    if (selectedId === null) {
      setDetail(null);
      return;
    }
    api
      .getFileDetail(selectedId)
      .then((next) => {
        if (!cancelled) setDetail(next);
      })
      .catch(() => {
        if (!cancelled) setDetail(null);
      });
    return () => {
      cancelled = true;
    };
  }, [selectedId, detailTick]);

  useEffect(() => {
    if (selectedId === null && files.length > 0) setSelectedId(files[0].id);
  }, [files, selectedId]);

  // Re-read the detail once the collector finishes a scan, so freshly computed
  // fingerprints show up without the user clicking anything.
  const lastScanState = useRef(status.scanState);
  useEffect(() => {
    const previous = lastScanState.current;
    lastScanState.current = status.scanState;
    if (previous !== status.scanState && status.scanState === "ready") {
      setDetailTick((tick) => tick + 1);
    }
  }, [status.scanState]);

  const onRescan = useCallback(async () => {
    try {
      await api.rescan();
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const visibleFiles = useMemo(() => files, [files]);
  const collecting = status.scanState === "scanning" || status.scanState === "hashing";

  const scanLabel =
    status.scanState === "scanning"
      ? `扫描中 · 已处理 ${status.scanDone.toLocaleString()}`
      : status.scanState === "hashing"
        ? `指纹计算中 · 待处理 ${status.pendingHash.toLocaleString()}`
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
            <p>FILELINEAGE · {status.elevated ? "管理员" : "标准权限"}</p>
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
            <span className="pending-pill">指纹队列 {status.pendingHash.toLocaleString()}</span>
          ) : null}
        </div>
        <div className="topbar-actions">
          <span className="version-tag">v{APP_VERSION}</span>
          <button className="btn" onClick={() => void onRescan()} disabled={collecting}>
            {collecting ? "采集中…" : "重新扫描"}
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
              {status.dirs.length === 0 ? (
                <li className="muted">0 个目录可监控（macOS 需在「系统设置 → 隐私与安全性」中授权）</li>
              ) : null}
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
                      const sum = status.attributionHits + status.attributionMisses;
                      return sum === 0 ? 0 : (status.attributionHits / sum) * 100;
                    })()}%`,
                  }}
                />
              </div>
              <span className="muted">
                {status.attributionHits}/{status.attributionHits + status.attributionMisses}
              </span>
            </div>
            <ul className="meta-list">
              <li>
                <span>ETW 内核事件流</span>
                <span className="mono">{status.etwState}</span>
              </li>
              <li>
                <span>ETW 归因事件</span>
                <span className="mono">{status.etwEvents.toLocaleString()}</span>
              </li>
              <li>
                <span>文件身份 (FileID)</span>
                <span className="mono">{status.identifiedFiles.toLocaleString()}</span>
              </li>
              <li>
                <span>分块总数</span>
                <span className="mono">{status.chunkCount.toLocaleString()}</span>
              </li>
              <li>
                <span>增量复用</span>
                <span className="mono">{status.incrementalHashes.toLocaleString()}</span>
              </li>
            </ul>
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
            <p className="muted small">
              主库 {formatBytes(status.dbSize)} · 合计 {formatBytes(status.totalDbSize)} · SQLite WAL
            </p>
            <ul className="meta-list">
              <li>
                <span>归档事件</span>
                <span className="mono">{status.archivedEvents.toLocaleString()}</span>
              </li>
              <li>
                <span>归档库</span>
                <span className="mono">{status.archiveFiles}</span>
              </li>
              <li>
                <span>Schema</span>
                <span className="mono">v{status.schemaVersion}</span>
              </li>
              <li>
                <span>开机自启</span>
                <span className={`mono ${status.autostart ? "" : "warn"}`}>
                  {status.autostart ? "已注册" : "未注册"}
                  {status.autostartConfigured !== status.autostart ? "（配置不一致）" : ""}
                </span>
              </li>
              <li>
                <span>打开记录</span>
                <span className="mono">{status.recentEntries.toLocaleString()}</span>
              </li>
            </ul>
            <div className="range-note">
              <strong>监控范围 · v{APP_VERSION}</strong>
              <p>
                递归监控 下载 / 桌面 / 文档 三个目录（含子目录），回答「从哪来 / 谁改过 / 删了会不会丢」。
              </p>
              <p>
                来源证据：下载器任务记录、Zone.Identifier、浏览器下载库、IM 接收目录、Shell 打开记录。
                Windows 另提供 ETW 与 Restart Manager 进程归因；macOS / Linux 的进程级归因不可用，结论会如实降级。
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
            <span className="result-count">
              {visibleFiles.length} / {total} 个文件
            </span>
          </div>
          <div className="filters">
            {FILTERS.map((f) => (
              <button
                key={f.key}
                className={`chip ${filter === f.key ? "active" : ""}`}
                onClick={() => {
                  setFilter(f.key);
                  setPage(0);
                }}
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
                        <span className="ext-dot">
                          {truncateChars(f.ext.toUpperCase(), 3) || "···"}
                        </span>
                        <div>
                          <div className="file-name" title={f.path}>
                            {f.name}
                          </div>
                          <div className="file-path" title={f.path}>
                            {shortPath(f.path)}
                          </div>
                        </div>
                      </div>
                    </td>
                    <td>
                      <SourceBadge kind={f.sourceKind} />
                    </td>
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
            {pageCount > 1 ? (
              <div className="pager">
                <button
                  className="btn tiny"
                  onClick={() => setPage((p) => Math.max(0, p - 1))}
                  disabled={page === 0}
                >
                  上一页
                </button>
                <span className="muted small">
                  第 {page + 1} / {pageCount} 页
                </span>
                <button
                  className="btn tiny"
                  onClick={() => setPage((p) => Math.min(pageCount - 1, p + 1))}
                  disabled={page >= pageCount - 1}
                >
                  下一页
                </button>
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
                  <span className="mono muted">
                    BLAKE3{" "}
                    {detail.file.contentHash
                      ? `${truncateChars(detail.file.contentHash, 13)}`
                      : "计算中"}
                  </span>
                  <span className="mono muted">证据 {detail.file.sourceEvidence || "none"}</span>
                  {detail.file.openedTs > 0 ? (
                    <span className="mono muted">最近打开 {formatTime(detail.file.openedTs)}</span>
                  ) : null}
                  <button className="btn tiny" onClick={() => void api.reveal(detail.file.path)}>
                    在文件管理器中显示
                  </button>
                </div>
              </div>

              <AnswerCard index="Q1" question="它从哪来？" answer={detail.answers.origin} tone="tone-origin" />
              <AnswerCard index="Q2" question="谁改过它？" answer={detail.answers.modifiers} tone="tone-process" />
              <AnswerCard index="Q3" question="删了会丢东西吗？" answer={detail.answers.safety} tone="tone-safety" />

              <VerdictCard verdict={detail.verdict} />

              <section className="panel">
                <div className="panel-head">
                  <h2>血缘视图</h2>
                  <span className="muted small">
                    {detail.graph.nodes.length} 节点 / {detail.graph.edges.length} 边
                  </span>
                </div>
                <LineageGraph graph={detail.graph} />
                <div className="legend">
                  {graphLegend.map((item) => (
                    <span key={item.key}>
                      <i style={{ background: item.css }} />
                      {item.label}
                    </span>
                  ))}
                </div>
              </section>

              <section className="panel">
                <div className="panel-head">
                  <h2>变更时间线</h2>
                  <span className="muted small">最近 {detail.versions.length} 条</span>
                </div>
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
                            <span className="proc-tag">
                              {ev.processName}
                              {ev.pid ? ` · PID ${ev.pid}` : ""}
                            </span>
                          ) : (
                            <span className="muted small">无进程归因</span>
                          )}
                          <span
                            className={`attr-badge ${
                              isInferredAttribution(ev.attribution) ? "attr-inferred" : ""
                            }`}
                          >
                            {attributionLabel[ev.attribution] ?? ev.attribution}
                          </span>
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
                  <div className="panel-head">
                    <h2>相同内容的其他位置</h2>
                  </div>
                  <ul className="copy-list">
                    {detail.copies.map((c) => (
                      <li key={c.id} className="mono small break">
                        {c.hardlink ? <span className="attr-badge">硬链接</span> : null}
                        {c.path}
                      </li>
                    ))}
                  </ul>
                </section>
              ) : null}

              {detail.related.length > 0 ? (
                <section className="panel">
                  <div className="panel-head">
                    <h2>部分相同的关联文件</h2>
                    <span className="muted small">共享分块，不是副本</span>
                  </div>
                  <ul className="copy-list">
                    {detail.related.map((r) => (
                      <li key={r.id} className="mono small break">
                        {r.path}（共享 {r.sharedChunks} 块）
                      </li>
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
