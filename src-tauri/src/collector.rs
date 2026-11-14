use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use notify::event::{ModifyKind, RenameMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use walkdir::WalkDir;

use crate::attrib::{who_holds, Attribution};
use crate::config::Config;
use crate::db::{FileObservation, SourceInfo, Store};
use crate::etw::{self, EtwIndex, EtwStart};
use crate::fingerprint::{digest_file, HashError, MAX_HASH_BYTES};
use crate::model::Status;
use crate::platform;
use crate::recent::Recent;
use crate::sources::{classify_path, refine_with_process, resolve, Sources};
use crate::util::{is_noise_file, now_ms, should_skip_dir};

/// 0.4.0: a full-disk index needs bigger batches, a deeper walk and a
/// configurable cap. `MAX_SCAN_FILES_DEFAULT` matches `Config::max_scan_files`.
const SCAN_BATCH: usize = 2_000;
const MAX_SCAN_DEPTH: usize = 32;
const MAX_SCAN_FILES_DEFAULT: i64 = 1_000_000;
/// The status cache is invalidated at most once per 20k registered files;
/// doing it per batch made the 5 s TTL meaningless during the first scan.
const STATS_EVERY: i64 = 20_000;
const HASH_BATCH: i64 = 32;
const DEBOUNCE: Duration = Duration::from_millis(400);
const PROBE_INTERVAL: Duration = Duration::from_millis(250);
/// How long an ETW observation stays authoritative for a path.
const ETW_FRESH_MS: i64 = 15_000;
const MAX_PENDING: usize = 8_000;
/// Nightly convergence: one incremental walk + `exists()` sweep per 24 h.
const RESWEEP_INTERVAL_MS: i64 = 86_400_000;
const GONE_BATCH: usize = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChangeKind {
    Created,
    Modified,
    Removed,
    Renamed,
}

impl ChangeKind {
    fn as_str(self) -> &'static str {
        match self {
            ChangeKind::Created => "created",
            ChangeKind::Modified => "modified",
            ChangeKind::Removed => "removed",
            ChangeKind::Renamed => "renamed",
        }
    }
}

struct Pending {
    kind: ChangeKind,
    note: Option<String>,
}

impl Pending {
    fn new(kind: ChangeKind, note: Option<String>) -> Self {
        Self { kind, note }
    }
}

pub struct Collector {
    pub store: Arc<Store>,
    pub status: Arc<Mutex<Status>>,
    /// Hot dirs: fingerprints + real-time events.
    pub dirs: Vec<PathBuf>,
    /// Roots the background indexer walks (fixed drives or `/`).
    index_roots: Vec<PathBuf>,
    sources: Mutex<Sources>,
    recent: Mutex<Recent>,
    etw: Arc<EtwIndex>,
    config: Mutex<Config>,
    running: AtomicBool,
    started: AtomicBool,
}

impl Collector {
    pub fn new(
        store: Arc<Store>,
        status: Arc<Mutex<Status>>,
        dirs: Vec<PathBuf>,
        index_roots: Vec<PathBuf>,
        config: Config,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            status,
            dirs,
            index_roots,
            sources: Mutex::new(Sources::default()),
            recent: Mutex::new(Recent::default()),
            etw: EtwIndex::new(),
            config: Mutex::new(config),
            running: AtomicBool::new(false),
            started: AtomicBool::new(false),
        })
    }

    pub fn start(self: &Arc<Self>) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        self.running.store(true, Ordering::SeqCst);

        // A failed `spawn` used to be swallowed by `.ok()`, which left the
        // daemon running with a dead channel and no warning anywhere. Every
        // failure now reaches `Status::last_error` and `startup.log`.
        let me = Arc::clone(self);
        if let Err(error) = std::thread::Builder::new()
            .name("fl-watcher".into())
            .spawn(move || me.watch_loop())
        {
            self.report_error(&format!("无法启动文件监控线程：{}", error));
        }

        let me = Arc::clone(self);
        if let Err(error) = std::thread::Builder::new()
            .name("fl-hasher".into())
            .spawn(move || me.hash_loop())
        {
            self.report_error(&format!("无法启动指纹线程：{}", error));
        }

        let me = Arc::clone(self);
        if let Err(error) = std::thread::Builder::new()
            .name("fl-maintenance".into())
            .spawn(move || me.maintenance_loop())
        {
            self.report_error(&format!("无法启动维护线程：{}", error));
        }

        self.start_etw();
        self.rescan();
    }

    fn report_error(&self, message: &str) {
        crate::startup::log(message);
        self.update(|s| s.last_error = Some(message.to_string()));
    }

    /// Start the ETW kernel channel when configured. Failure is never fatal:
    /// the Restart Manager channel keeps working, and the reason is surfaced
    /// through `Status::etw_state` so the UI can explain it.
    fn start_etw(self: &Arc<Self>) {
        let enabled = self.config.lock().map(|c| c.enable_etw).unwrap_or(true);
        if !enabled || !cfg!(windows) {
            // A disabled switch and a platform without ETW produce distinct,
            // honest states.
            let state = if enabled {
                "unsupported: 本平台没有 ETW，进程级归因不可用"
            } else {
                "disabled"
            };
            self.update(|s| s.etw_state = state.into());
            return;
        }
        let elevated = crate::runtime::is_elevated();
        self.update(|s| s.elevated = elevated);

        match etw::start(Arc::clone(&self.etw)) {
            EtwStart::Active => self.update(|s| s.etw_state = "active".into()),
            EtwStart::NotElevated => self.update(|s| {
                s.etw_state = "denied: 需要管理员权限，当前使用 Restart Manager".into()
            }),
            // `Disabled`/`Unsupported` cannot be produced through this path any
            // more (handled above), but the match must stay exhaustive.
            EtwStart::Disabled => self.update(|s| s.etw_state = "disabled".into()),
            EtwStart::Unsupported => self.update(|s| {
                s.etw_state = "unsupported: 本平台没有 ETW，进程级归因不可用".into()
            }),
            EtwStart::Failed(reason) => self.update(|s| s.etw_state = format!("error: {}", reason)),
        }
    }

    pub fn rescan(self: &Arc<Self>) {
        let me = Arc::clone(self);
        if let Err(error) = std::thread::Builder::new()
            .name("fl-indexer".into())
            .spawn(move || {
                // v3 databases registered every file as `settling`. Now that
                // the index covers the whole disk, only hot-dir rows may enter
                // the hash queue. Running it on this thread (not the Tauri
                // setup thread) keeps a million-row UPDATE off startup.
                me.store.rescope_legacy_hot(&me.dirs);
                me.reload_sources();
                me.scan_once();
                me.recent_backfill();
                me.download_backfill();
            })
        {
            self.report_error(&format!("无法启动扫描线程：{}", error));
        }
    }

    #[allow(dead_code)]
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    fn update<F: FnOnce(&mut Status)>(&self, f: F) {
        if let Ok(mut status) = self.status.lock() {
            f(&mut status);
        }
    }

    fn reload_sources(&self) {
        let loaded = Sources::load();
        let (browser_adapters, records) = (
            loaded.adapter_names().to_vec(),
            loaded.record_count() as i64,
        );
        if let Ok(mut guard) = self.sources.lock() {
            *guard = loaded;
        }

        let recent = Recent::load();
        let recent_count = recent.entry_count() as i64;
        let recent_loaded = recent.is_loaded();
        if let Ok(mut guard) = self.recent.lock() {
            *guard = recent;
        }

        let mut adapters = vec!["Zone.Identifier 来源流（与浏览器无关的下载证据）".to_string()];
        if recent_loaded {
            adapters.push(format!("Shell「最近使用」打开记录（{} 条）", recent_count));
        }
        adapters.extend(browser_adapters);
        adapters.push("IM 接收目录布局（账号 + 接收月份）".to_string());

        self.update(|s| {
            s.adapters = adapters.clone();
            s.source_records = records;
            s.recent_entries = recent_count;
        });
    }

    fn resolve_source(
        &self,
        path: &str,
        name: &str,
        size: i64,
        probe: Option<&Attribution>,
    ) -> SourceInfo {
        let mut info = match self.sources.lock() {
            Ok(guard) => resolve(&guard, path, name, size),
            Err(_) => classify_path(path),
        };
        if let Some(process) = probe.and_then(|a| a.process_name.as_deref()) {
            info = refine_with_process(info, process);
        }
        info
    }

    /// Attribute a change to a process.
    ///
    /// ETW wins when it has something fresh for this path: it is the actual
    /// writing PID. Restart Manager is the fallback and is only useful *while*
    /// the writer still holds the handle, so it is probed on raw events and
    /// rate-limited rather than on the debounced flush.
    fn probe(
        &self,
        path: &Path,
        probes: &mut HashMap<PathBuf, Attribution>,
        last_probe: &mut HashMap<PathBuf, Instant>,
    ) {
        let key = path.to_string_lossy().to_string();
        if let Some(attribution) = self.etw.take(&key, ETW_FRESH_MS) {
            self.update(|s| s.etw_events += 1);
            probes.insert(path.to_path_buf(), attribution);
            return;
        }
        if probes.contains_key(path) {
            return;
        }
        let too_soon = last_probe
            .get(path)
            .map(|t| t.elapsed() < PROBE_INTERVAL)
            .unwrap_or(false);
        if too_soon {
            return;
        }
        last_probe.insert(path.to_path_buf(), Instant::now());
        if let Some(hit) = who_holds(path).into_iter().next() {
            probes.insert(path.to_path_buf(), hit);
        }
    }

    /// Last chance to attribute a change, called just before the write.
    fn final_probe(&self, path: &Path, cached: Option<Attribution>) -> Option<Attribution> {
        if cached.as_ref().is_some_and(|a| a.is_usable()) {
            return cached;
        }
        let key = path.to_string_lossy().to_string();
        if let Some(attribution) = self.etw.take(&key, ETW_FRESH_MS) {
            self.update(|s| s.etw_events += 1);
            return Some(attribution);
        }
        who_holds(path)
            .into_iter()
            .find(|a| a.is_usable())
            .or(cached)
    }

    // ---------------------------------------------------------------- scan

    /// Is this path inside the fingerprint / real-time (hot) scope?
    fn is_hot(&self, path: &Path) -> bool {
        self.dirs.iter().any(|dir| path.starts_with(dir))
    }

    /// Flush one scope's batch in a single transaction. Errors go to both the
    /// status bar and startup.log — a silently half-indexed disk is worse than
    /// a visible warning.
    fn flush_batch(&self, batch: &mut Vec<(FileObservation, SourceInfo)>, scope: &str) -> i64 {
        if batch.is_empty() {
            return 0;
        }
        let n = batch.len() as i64;
        if let Err(error) = self.store.register_batch_scoped(batch, scope) {
            let message = format!("索引写入失败（{} 区）：{}", scope, error);
            crate::startup::log(&message);
            self.update(|s| s.last_error = Some(message));
        }
        batch.clear();
        n
    }

    /// Walk every index root once. `announce` distinguishes the startup pass
    /// (drives the UI progress state) from the nightly incremental resweep
    /// (same work, quiet). Returns `true` when `max_scan_files` was reached;
    /// the caller keeps that flag in the ready state so the UI can say so.
    fn walk_index(&self, announce: bool) -> bool {
        if announce {
            self.update(|s| {
                s.scan_state = "indexing".into();
                s.index_state = "indexing".into();
                s.scan_done = 0;
                s.scan_total = 0;
                s.scan_truncated = false;
            });
        }
        let max_files = self
            .config
            .lock()
            .map(|c| c.max_scan_files)
            .unwrap_or(MAX_SCAN_FILES_DEFAULT)
            .max(1);
        let exclusions = platform::index_exclusions();
        let mut hot_batch: Vec<(FileObservation, SourceInfo)> = Vec::with_capacity(SCAN_BATCH);
        let mut cold_batch: Vec<(FileObservation, SourceInfo)> = Vec::with_capacity(SCAN_BATCH);
        let mut processed: i64 = 0;
        let mut last_invalidate: i64 = 0;
        let mut truncated = false;

        'roots: for root in &self.index_roots {
            if !self.running.load(Ordering::Relaxed) {
                break;
            }
            if !root.is_dir() {
                continue;
            }
            let walker = WalkDir::new(root)
                .max_depth(MAX_SCAN_DEPTH)
                .follow_links(false)
                .into_iter()
                .filter_entry(|e| {
                    if e.depth() == 0 {
                        return true;
                    }
                    let name = e.file_name().to_string_lossy();
                    !should_skip_dir(&name)
                        && !exclusions.iter().any(|prefix| e.path().starts_with(prefix))
                });

            for entry in walker.filter_map(|e| e.ok()) {
                if !self.running.load(Ordering::Relaxed) {
                    break 'roots;
                }
                if processed >= max_files {
                    truncated = true;
                    break 'roots;
                }
                if !entry.file_type().is_file() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if is_noise_file(&name) {
                    continue;
                }
                let Some(obs) = FileObservation::from_path(entry.path()) else {
                    continue;
                };
                if self.is_hot(entry.path()) {
                    let src = self.resolve_source(&obs.path, &obs.name, obs.size, None);
                    hot_batch.push((obs, src));
                } else {
                    // Path-shape classification is enough for index-only rows;
                    // running every adapter per cold file would dominate a
                    // million-file walk for no evidentiary gain.
                    let src = classify_path(&obs.path);
                    cold_batch.push((obs, src));
                }
                if hot_batch.len() + cold_batch.len() >= SCAN_BATCH {
                    processed += self.flush_batch(&mut hot_batch, "hot");
                    processed += self.flush_batch(&mut cold_batch, "cold");
                    if processed - last_invalidate >= STATS_EVERY {
                        self.store.invalidate_stats();
                        last_invalidate = processed;
                    }
                    if announce {
                        self.update(|s| s.scan_done = processed);
                    }
                }
            }
        }

        processed += self.flush_batch(&mut hot_batch, "hot");
        processed += self.flush_batch(&mut cold_batch, "cold");
        self.store.invalidate_stats();
        if announce {
            self.update(|s| {
                s.scan_done = processed;
                s.scan_total = processed;
                s.scan_state = "ready".into();
                s.index_state = "done".into();
                // Kept in the ready state on purpose: "采集就绪" must not hide
                // that the index stopped at the configured cap.
                s.scan_truncated = truncated;
            });
        }
        truncated
    }

    fn scan_once(&self) {
        self.walk_index(true);
        self.store.checkpoint();
    }

    /// Nightly convergence: re-walk the roots incrementally (the upsert is a
    /// no-op for unchanged metadata), then mark every live DB row whose path
    /// no longer exists. Watch events are the fast path; this is the only place
    /// a full-disk index can converge `liveness` after a crash or overflow.
    fn nightly_resweep(&self) {
        self.walk_index(false);
        self.prune_gone();
        self.store.checkpoint();
        self.store.invalidate_stats();
        crate::startup::log("nightly incremental resweep finished");
    }

    fn prune_gone(&self) {
        let mut after = 0i64;
        let mut gone: Vec<i64> = Vec::with_capacity(GONE_BATCH);
        loop {
            if !self.running.load(Ordering::Relaxed) {
                return;
            }
            let rows = self.store.live_paths_after(after, GONE_BATCH as i64);
            if rows.is_empty() {
                break;
            }
            for (id, path) in &rows {
                if !Path::new(path).exists() {
                    gone.push(*id);
                }
            }
            after = rows.last().map(|(id, _)| *id).unwrap_or(after + 1);
            if gone.len() >= GONE_BATCH {
                if let Err(error) = self.store.mark_gone_batch(&gone) {
                    self.report_error(&format!("批量标记消失文件失败：{}", error));
                }
                gone.clear();
            }
        }
        if !gone.is_empty() {
            if let Err(error) = self.store.mark_gone_batch(&gone) {
                self.report_error(&format!("批量标记消失文件失败：{}", error));
            }
        }
    }

    /// Give already-known files their "opened" timeline by matching the shell's
    /// Recent shortcuts. Bounded by the number of shortcuts, not by file count.
    fn recent_backfill(&self) {
        let entries = match self.recent.lock() {
            Ok(guard) => guard.entries(),
            Err(_) => return,
        };
        if entries.is_empty() {
            return;
        }
        let mut matched = 0i64;
        for (name, ts, shortcut_count) in entries {
            if !self.running.load(Ordering::Relaxed) {
                return;
            }
            for (id, _path) in self.store.ids_by_name(&name, "", 0) {
                let note = format!(
                    "Shell「最近使用」记录（{} 个快捷方式）。打开者不会出现在文件写入事件里，因此这是推断信号。",
                    shortcut_count
                );
                match self.store.record_opened(id, ts, &note) {
                    Ok(true) => matched += 1,
                    Ok(false) => {}
                    Err(error) => {
                        self.report_error(&format!("写入打开记录失败：{}", error));
                    }
                }
            }
        }
        if matched > 0 {
            let ts = now_ms();
            self.update(|s| {
                s.events_this_session += matched;
                s.last_event_at = ts;
            });
        }
    }

    /// Backfill the timeline with downloader task records. This is what makes
    /// files downloaded *before* installation answer "how was I produced":
    /// the task log carries the tool, the URL and the completion time even
    /// though we never observed the write.
    fn download_backfill(&self) {
        let records = match self.sources.lock() {
            Ok(guard) => guard.downloaders.records().to_vec(),
            Err(_) => return,
        };
        if records.is_empty() {
            return;
        }
        let mut added = 0i64;
        for record in records {
            if !self.running.load(Ordering::Relaxed) {
                return;
            }
            let ts = record.timestamp();
            if ts <= 0 {
                continue;
            }
            let mut ids = Vec::new();
            // Exact-path matches must be in the fingerprint scope too: the
            // whole point of B2-6 is that a full-disk index must not attach
            // "opened"/"downloaded" events to unrelated same-named files.
            if let Some(id) = self.store.id_for_path_in_scope(&record.path) {
                ids.push(id);
            }
            if ids.is_empty() {
                // Narrow by extension and size as well: a name-only match used
                // to attach a download record to every same-named file.
                let ext = crate::util::extension_of(&record.name);
                for (id, _) in self.store.ids_by_name(&record.name, &ext, record.size) {
                    ids.push(id);
                }
            }
            let note = format!(
                "由 {} 下载并写入本机。\n来源：{}\n任务：{}",
                record.vendor,
                crate::downloaders::truncate(&record.url, 220),
                crate::downloaders::truncate(&record.task_id, 64)
            );
            for id in ids {
                match self.store.record_download(id, ts, &note) {
                    Ok(true) => added += 1,
                    Ok(false) => {}
                    Err(error) => {
                        self.report_error(&format!("写入下载器任务事件失败：{}", error));
                    }
                }
            }
        }
        if added > 0 {
            let ts = now_ms();
            self.update(|s| {
                s.events_this_session += added;
                s.last_event_at = ts;
            });
        }
    }

    /// Name of the downloader that produced this path, if a task log says so.
    fn downloader_for(&self, path: &str, name: &str, size: i64) -> Option<String> {
        let guard = self.sources.lock().ok()?;
        guard
            .downloaders
            .lookup(path, name, size)
            .map(|record| record.vendor.clone())
    }

    // ------------------------------------------------------------- watching

    fn watch_loop(&self) {
        let (tx, rx) = mpsc::channel::<notify::Result<Event>>();
        let mut watcher: RecommendedWatcher = match notify::recommended_watcher(move |res| {
            let _ = tx.send(res);
        }) {
            Ok(w) => w,
            Err(e) => {
                self.update(|s| {
                    s.watching = false;
                    s.last_error = Some(format!("无法启动文件监控：{}", e));
                });
                return;
            }
        };

        let (watch_targets, mut scope) = platform::watch_roots(&self.dirs, &self.index_roots);
        let mut watched = 0usize;
        for dir in &watch_targets {
            if watcher.watch(dir, RecursiveMode::Recursive).is_ok() {
                watched += 1;
            }
        }
        if watched == 0 && !self.dirs.is_empty() {
            // A full-volume watch can be refused (FSEvents permission, inotify
            // limit). Falling back to the hot dirs keeps lineage alive, and the
            // scope text must say so instead of claiming whole-disk coverage.
            for dir in &self.dirs {
                if watcher.watch(dir, RecursiveMode::Recursive).is_ok() {
                    watched += 1;
                }
            }
            scope = "仅指纹范围目录（全盘 watch 启动失败，其余依赖每夜重扫）".to_string();
        }
        self.update(|s| {
            s.watching = watched > 0;
            s.watch_scope = scope.clone();
            if watched == 0 {
                s.last_error = Some("没有可监控的目录".into());
            }
        });

        let exclusions = platform::index_exclusions();
        let is_excluded = |p: &Path| exclusions.iter().any(|prefix| p.starts_with(prefix));
        // Hot paths keep the full probe → event → settle pipeline. Cold paths
        // only refresh the index row, so a whole-volume watch does not turn
        // `RmGetList` into a hot loop on every `npm install` (B2-7).
        let mut pending: HashMap<PathBuf, Pending> = HashMap::new();
        let mut cold_pending: HashMap<PathBuf, ChangeKind> = HashMap::new();
        let mut probes: HashMap<PathBuf, Attribution> = HashMap::new();
        let mut last_probe: HashMap<PathBuf, Instant> = HashMap::new();
        let mut quiet_since: Option<Instant> = None;

        while self.running.load(Ordering::Relaxed) {
            match rx.recv_timeout(Duration::from_millis(120)) {
                Ok(Ok(event)) => {
                    let Some(kind) = classify_event(&event) else {
                        continue;
                    };
                    if event.paths.is_empty() {
                        continue;
                    }

                    // A rename carries [old, new] in one event. Handling both
                    // sides preserves short-term identity without relying on
                    // inode lookups.
                    if kind == ChangeKind::Renamed && event.paths.len() >= 2 {
                        let old = event.paths[0].clone();
                        let new = event.paths[1].clone();
                        for (path, change) in [
                            (old.clone(), ChangeKind::Removed),
                            (new, ChangeKind::Renamed),
                        ] {
                            if is_excluded(&path) {
                                continue;
                            }
                            if self.is_hot(&path) {
                                let note = if change == ChangeKind::Renamed {
                                    Some(format!("由 {} 重命名而来", old.display()))
                                } else {
                                    None
                                };
                                pending.insert(path.clone(), Pending::new(change, note));
                                if change != ChangeKind::Removed {
                                    self.probe(&path, &mut probes, &mut last_probe);
                                }
                            } else {
                                cold_pending.insert(path, change);
                            }
                        }
                        quiet_since = Some(Instant::now());
                        continue;
                    }

                    let mut force_flush = false;
                    for path in &event.paths {
                        if is_excluded(path) {
                            continue;
                        }
                        if pending.len() + cold_pending.len() >= MAX_PENDING {
                            // Back-pressure: do not wait for a quiet window
                            // when the pending maps are already full.
                            force_flush = true;
                            break;
                        }
                        if self.is_hot(path) {
                            let entry = pending
                                .entry(path.clone())
                                .or_insert_with(|| Pending::new(kind, None));
                            // "created" is strictly more informative than a
                            // later "modified" in the same burst, so never
                            // downgrade it.
                            if entry.kind == ChangeKind::Modified && kind == ChangeKind::Created {
                                entry.kind = ChangeKind::Created;
                            }
                            if kind != ChangeKind::Removed {
                                self.probe(path, &mut probes, &mut last_probe);
                            }
                        } else {
                            let keep_removal = cold_pending
                                .get(path)
                                .is_some_and(|existing| *existing == ChangeKind::Removed)
                                && kind != ChangeKind::Removed;
                            if !keep_removal {
                                cold_pending.insert(path.clone(), kind);
                            }
                        }
                    }
                    quiet_since = Some(if force_flush {
                        Instant::now() - DEBOUNCE
                    } else {
                        Instant::now()
                    });
                }
                Ok(Err(e)) => {
                    self.update(|s| s.last_error = Some(format!("监控事件错误：{}", e)));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }

            if (!pending.is_empty() || !cold_pending.is_empty())
                && quiet_since
                    .map(|t| t.elapsed() >= DEBOUNCE)
                    .unwrap_or(false)
            {
                let cold_items: Vec<(PathBuf, ChangeKind)> = cold_pending.drain().collect();
                for (path, change) in cold_items {
                    self.apply_cold_change(&path, change);
                }
                let items: Vec<(PathBuf, Pending)> = pending.drain().collect();
                for (path, change) in items {
                    let probe = probes.remove(&path);
                    self.apply_change(&path, change.kind, change.note.as_deref(), probe);
                }
                quiet_since = None;
                if last_probe.len() > 4_096 {
                    last_probe.clear();
                    probes.clear();
                }
            }
        }

        drop(watcher);
        self.update(|s| s.watching = false);
    }

    /// Cold zone: index-only. No Restart Manager probe, no `final_probe`, no
    /// events and no "opened" backfill. Deletions still need `mark_gone` so the
    /// list does not keep showing a file that no longer exists.
    fn apply_cold_change(&self, path: &Path, kind: ChangeKind) {
        let key = path.to_string_lossy().to_string();
        if kind == ChangeKind::Removed {
            if let Err(error) = self.store.mark_gone(&key) {
                self.report_error(&format!("标记文件消失失败：{}", error));
            }
            return;
        }
        let Some(obs) = FileObservation::from_path(path) else {
            return;
        };
        if is_noise_file(&obs.name) {
            return;
        }
        if let Err(error) = self
            .store
            .upsert_file_scoped(&obs, &classify_path(&obs.path), "cold")
        {
            self.report_error(&format!("写入索引记录失败：{}", error));
        }
    }

    fn apply_change(
        &self,
        path: &Path,
        kind: ChangeKind,
        note: Option<&str>,
        probe: Option<Attribution>,
    ) {
        if kind == ChangeKind::Removed {
            let key = path.to_string_lossy().to_string();
            if let Some(id) = self.store.id_for_path(&key) {
                if let Err(error) = self.store.record_event(
                    id,
                    "removed",
                    now_ms(),
                    0,
                    None,
                    Some("文件已从指纹范围目录中消失"),
                ) {
                    self.report_error(&format!("记录删除事件失败：{}", error));
                }
            }
            if let Err(error) = self.store.mark_gone(&key) {
                self.report_error(&format!("标记文件消失失败：{}", error));
            }
            return;
        }

        let Some(obs) = FileObservation::from_path(path) else {
            return;
        };
        if is_noise_file(&obs.name) {
            return;
        }
        let mut probe = self.final_probe(path, probe);
        if probe.is_none() {
            // A downloader task log names the producing tool even when the
            // write handle is long gone (common for rename-on-completion).
            if let Some(vendor) = self.downloader_for(&obs.path, &obs.name, obs.size) {
                probe = Some(Attribution {
                    process_name: Some(vendor),
                    process_path: None,
                    pid: None,
                    method: "download-task".to_string(),
                });
            }
        }

        let mut notes: Vec<String> = Vec::new();
        if let Some(text) = note {
            notes.push(text.to_string());
        }
        if let Some((previous_volume, previous_index)) = self.store.identity_for_path(&obs.path) {
            let replaced = match (previous_volume, previous_index, obs.volume, obs.file_index) {
                (Some(old_volume), Some(old_index), Some(new_volume), Some(new_index)) => {
                    old_volume != new_volume || old_index != new_index
                }
                _ => false,
            };
            if replaced {
                notes.push("路径上的文件已被替换：FileID 变化，说明这不是同一个文件".to_string());
            }
        }

        let src = self.resolve_source(&obs.path, &obs.name, obs.size, probe.as_ref());

        // H5: reconnect a rename/move through file identity.
        //
        // Relying on a two-path rename event does not work across platforms:
        // the Windows backend only emits `From` and `To`, macOS emits a single
        // `Modify(Name(Any))` with just the new path, and Linux emits both. A
        // `(volume_serial, file_index)` match against a row that is currently
        // `gone` is the portable signal, and it keeps the file's whole lineage
        // (events, chunks, source evidence) attached to the new path.
        let reconnected = self.store.reconnect_identity(&obs, &src);
        let id = match &reconnected {
            Some((existing, previous_path)) => {
                notes.push(format!(
                    "由 {} 重命名/移动而来（FileID 未变，血缘已续接）",
                    previous_path
                ));
                *existing
            }
            None => match self.store.upsert_file(&obs, &src) {
                Ok(id) => id,
                Err(error) => {
                    self.report_error(&format!("写入文件记录失败：{}", error));
                    return;
                }
            },
        };
        self.store.invalidate_stats();
        let combined = if notes.is_empty() {
            None
        } else {
            Some(notes.join("；"))
        };
        // A reconnected change *is* a rename, whatever the raw event said.
        let event_kind = if reconnected.is_some() {
            "renamed"
        } else {
            kind.as_str()
        };

        let attributed = probe.as_ref().filter(|a| a.is_usable());
        let hit = attributed.is_some();
        let recorded = match self
            .store
            .record_event(
                id,
                event_kind,
                now_ms(),
                obs.size,
                attributed,
                combined.as_deref(),
            ) {
            Ok(recorded) => recorded,
            Err(error) => {
                self.report_error(&format!("写入变更事件失败：{}", error));
                false
            }
        };
        if recorded {
            let ts = now_ms();
            self.update(|s| {
                s.events_this_session += 1;
                s.last_event_at = ts;
                if hit {
                    s.attribution_hits += 1;
                } else {
                    s.attribution_misses += 1;
                }
            });
        }

        self.observe_opened(id, &obs.name);
    }

    /// Fold the shell's Recent shortcut for this file name into the timeline.
    fn observe_opened(&self, file_id: i64, name: &str) {
        let (ts, count) = match self.recent.lock() {
            Ok(guard) => (guard.newest_for(name), guard.shortcut_count(name)),
            Err(_) => return,
        };
        let Some(ts) = ts else { return };
        if ts <= 0 {
            return;
        }
        let note = format!(
            "Shell「最近使用」记录（{} 个快捷方式）。打开者不会出现在文件写入事件里，因此这是推断信号。",
            count
        );
        if let Err(error) = self.store.record_opened(file_id, ts, &note) {
            self.report_error(&format!("写入打开记录失败：{}", error));
            return;
        }
        self.update(|s| s.events_this_session += 1);
    }

    // -------------------------------------------------------------- hashing

    fn hash_loop(&self) {
        let mut window_start = Instant::now();
        let mut window_chunks: i64 = 0;

        while self.running.load(Ordering::Relaxed) {
            let (settle_seconds, budget, incremental) = match self.config.lock() {
                Ok(config) => (
                    config.settle_seconds,
                    config.hash_chunks_per_minute,
                    config.incremental_hash,
                ),
                Err(_) => (20, 9_000, true),
            };

            // Only files that have stopped changing enter the queue.
            if let Err(error) = self.store.promote_settled(now_ms(), settle_seconds) {
                self.report_error(&format!("指纹队列推进失败：{}", error));
            }

            if window_start.elapsed() >= Duration::from_secs(60) {
                window_start = Instant::now();
                window_chunks = 0;
            }
            if window_chunks >= budget {
                std::thread::sleep(Duration::from_millis(2_000));
                continue;
            }

            let batch = self.store.pending_hash_batch(HASH_BATCH);
            if batch.is_empty() {
                let mut queue_drained = false;
                self.update(|s| {
                    if s.scan_state == "hashing" {
                        s.scan_state = "ready".into();
                        queue_drained = true;
                    }
                });
                if queue_drained {
                    self.store.invalidate_stats();
                }
                std::thread::sleep(Duration::from_millis(1_500));
                continue;
            }
            self.update(|s| {
                // An indexing pass is still a scan: the hash thread must not
                // flip the label to "hashing" (or "ready") while it runs.
                if s.scan_state != "scanning" && s.scan_state != "indexing" {
                    s.scan_state = "hashing".into();
                }
            });

            for (id, path, size) in batch {
                if !self.running.load(Ordering::Relaxed) {
                    return;
                }
                if size < 0 || size as u64 > MAX_HASH_BYTES {
                    if let Err(error) = self.store.set_hash_state(id, "too_large") {
                        self.report_error(&format!("写入 too_large 状态失败：{}", error));
                    }
                    self.store.invalidate_stats();
                    continue;
                }
                let previous = if incremental {
                    self.store.chunks_for(id)
                } else {
                    Vec::new()
                };
                let reuse = if previous.is_empty() {
                    None
                } else {
                    Some(previous.as_slice())
                };
                match digest_file(Path::new(&path), reuse) {
                    Ok(digest) => {
                        let chunks = digest.chunks.len() as i64;
                        if let Err(error) = self.store.set_digest(
                            id,
                            &digest.root,
                            &digest.chunks,
                            digest.incremental,
                        ) {
                            // A persistent write failure here means the file
                            // never becomes comparable — it must be visible.
                            self.report_error(&format!("写入内容指纹失败：{}", error));
                            continue;
                        }
                        window_chunks += chunks.max(1);
                        self.update(|s| s.hashed_count += 1);
                        self.store.invalidate_stats();
                    }
                    Err(HashError::TooLarge) => {
                        if let Err(error) = self.store.set_hash_state(id, "too_large") {
                            self.report_error(&format!("写入 too_large 状态失败：{}", error));
                        }
                        self.store.invalidate_stats();
                    }
                    Err(HashError::Io(error)) => {
                        if let Err(write_error) = self.store.set_hash_state(id, "error") {
                            self.report_error(&format!("写入哈希错误状态失败：{}", write_error));
                        }
                        self.store.invalidate_stats();
                        crate::startup::log(&format!("哈希失败 {}：{}", path, error));
                    }
                }
            }
        }
    }

    // ---------------------------------------------------------- maintenance

    fn maintenance_loop(&self) {
        // The startup index owns the disk for its first pass; let it finish
        // before maintenance competes for the database.
        for _ in 0..6 {
            if !self.running.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_secs(30));
        }

        // A resident daemon crosses a night every 24 h; that is the only
        // reliable convergence point for `liveness` on cold zones and after a
        // watch buffer overflow (B2-12).
        let mut last_resweep = now_ms();
        loop {
            if !self.running.load(Ordering::Relaxed) {
                return;
            }
            let days = match self.config.lock() {
                Ok(config) => config.archive_after_days,
                Err(_) => 90,
            };
            let cutoff = now_ms() - days * 86_400_000;
            match self.store.archive_events(cutoff) {
                Ok(moved) if moved > 0 => {
                    let ts = now_ms();
                    self.update(|s| s.last_archive_at = ts);
                }
                Ok(_) => {}
                Err(e) => self.update(|s| s.last_error = Some(format!("归档失败：{}", e))),
            }

            let since = now_ms();
            if since - last_resweep >= RESWEEP_INTERVAL_MS {
                last_resweep = since;
                self.nightly_resweep();
            }

            for _ in 0..60 {
                if !self.running.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(Duration::from_secs(30));
            }
        }
    }
}

fn classify_event(event: &Event) -> Option<ChangeKind> {
    match event.kind {
        EventKind::Create(_) => Some(ChangeKind::Created),
        EventKind::Remove(_) => Some(ChangeKind::Removed),
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => Some(ChangeKind::Renamed),
        EventKind::Modify(ModifyKind::Name(RenameMode::From)) => Some(ChangeKind::Removed),
        EventKind::Modify(ModifyKind::Name(RenameMode::To)) => Some(ChangeKind::Created),
        // macOS FSEvents only reports the *new* path as `Name(Any)`; Linux
        // inotify sends extra `Name(Any)` events alongside `Both`. Treating
        // these as a plain `Modified` (the old behaviour) meant the new path was
        // never inserted on macOS, so the rename was lost entirely.
        EventKind::Modify(ModifyKind::Name(_)) => Some(ChangeKind::Renamed),
        EventKind::Modify(_) => Some(ChangeKind::Modified),
        EventKind::Access(_) => None,
        _ => Some(ChangeKind::Modified),
    }
}
