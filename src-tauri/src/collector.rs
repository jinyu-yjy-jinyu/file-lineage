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
use crate::recent::Recent;
use crate::sources::{classify_path, refine_with_process, resolve, Sources};
use crate::util::{is_noise_file, now_ms, should_skip_dir};

const SCAN_BATCH: usize = 400;
const HASH_BATCH: i64 = 32;
const DEBOUNCE: Duration = Duration::from_millis(400);
const PROBE_INTERVAL: Duration = Duration::from_millis(250);
/// How long an ETW observation stays authoritative for a path.
const ETW_FRESH_MS: i64 = 15_000;
const MAX_PENDING: usize = 8_000;
const MAX_SCAN_FILES: i64 = 200_000;

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
    pub dirs: Vec<PathBuf>,
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
        config: Config,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            status,
            dirs,
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

        let me = Arc::clone(self);
        std::thread::Builder::new()
            .name("fl-watcher".into())
            .spawn(move || me.watch_loop())
            .ok();

        let me = Arc::clone(self);
        std::thread::Builder::new()
            .name("fl-hasher".into())
            .spawn(move || me.hash_loop())
            .ok();

        let me = Arc::clone(self);
        std::thread::Builder::new()
            .name("fl-maintenance".into())
            .spawn(move || me.maintenance_loop())
            .ok();

        self.start_etw();
        self.rescan();
    }

    /// Start the ETW kernel channel when configured. Failure is never fatal:
    /// the Restart Manager channel keeps working, and the reason is surfaced
    /// through `Status::etw_state` so a future UI can explain it.
    fn start_etw(self: &Arc<Self>) {
        let enabled = self.config.lock().map(|c| c.enable_etw).unwrap_or(true);
        if !enabled {
            self.update(|s| s.etw_state = "disabled".into());
            return;
        }
        let elevated = crate::runtime::is_elevated();
        self.update(|s| s.elevated = elevated);

        match etw::start(Arc::clone(&self.etw)) {
            EtwStart::Active => self.update(|s| s.etw_state = "active".into()),
            EtwStart::NotElevated => self.update(|s| {
                s.etw_state = "denied: 需要管理员权限，当前使用 Restart Manager".into()
            }),
            EtwStart::Disabled => self.update(|s| s.etw_state = "disabled".into()),
            EtwStart::Failed(reason) => self.update(|s| s.etw_state = format!("error: {}", reason)),
        }
    }

    pub fn rescan(self: &Arc<Self>) {
        let me = Arc::clone(self);
        std::thread::Builder::new()
            .name("fl-scanner".into())
            .spawn(move || {
                me.reload_sources();
                me.scan_once();
                me.recent_backfill();
                me.download_backfill();
            })
            .ok();
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

    fn scan_once(&self) {
        self.update(|s| {
            s.scan_state = "scanning".into();
            s.scan_done = 0;
            s.scan_total = 0;
        });

        let mut batch: Vec<(FileObservation, SourceInfo)> = Vec::with_capacity(SCAN_BATCH);
        let mut processed: i64 = 0;

        for dir in &self.dirs {
            if !self.running.load(Ordering::Relaxed) {
                break;
            }
            if !dir.is_dir() {
                continue;
            }
            let walker = WalkDir::new(dir)
                .max_depth(10)
                .follow_links(false)
                .into_iter()
                .filter_entry(|e| {
                    let name = e.file_name().to_string_lossy();
                    e.depth() == 0 || !should_skip_dir(&name)
                });

            for entry in walker.filter_map(|e| e.ok()) {
                if !self.running.load(Ordering::Relaxed) || processed >= MAX_SCAN_FILES {
                    break;
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
                let src = self.resolve_source(&obs.path, &obs.name, obs.size, None);
                batch.push((obs, src));

                if batch.len() >= SCAN_BATCH {
                    let n = batch.len() as i64;
                    if let Err(e) = self.store.register_batch(&batch) {
                        self.update(|s| s.last_error = Some(format!("扫描写入失败：{}", e)));
                    }
                    processed += n;
                    batch.clear();
                    let done = processed;
                    self.update(|s| s.scan_done = done);
                }
            }
        }

        if !batch.is_empty() {
            let n = batch.len() as i64;
            if let Err(e) = self.store.register_batch(&batch) {
                self.update(|s| s.last_error = Some(format!("扫描写入失败：{}", e)));
            }
            processed += n;
        }
        self.update(|s| {
            s.scan_done = processed;
            s.scan_total = processed;
            s.scan_state = "ready".into();
        });
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
            for (id, _path) in self.store.ids_by_name(&name) {
                let note = format!(
                    "Shell「最近使用」记录（{} 个快捷方式）。打开者不会出现在文件写入事件里，因此这是推断信号。",
                    shortcut_count
                );
                if self.store.record_opened(id, ts, &note).unwrap_or(false) {
                    matched += 1;
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
            if let Some(id) = self.store.id_for_path(&record.path) {
                ids.push(id);
            }
            if ids.is_empty() {
                for (id, _) in self.store.ids_by_name(&record.name) {
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
                if self.store.record_download(id, ts, &note).unwrap_or(false) {
                    added += 1;
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

        let mut watched = 0usize;
        for dir in &self.dirs {
            if watcher.watch(dir, RecursiveMode::Recursive).is_ok() {
                watched += 1;
            }
        }
        self.update(|s| {
            s.watching = watched > 0;
            if watched == 0 {
                s.last_error = Some("没有可监控的目录".into());
            }
        });

        let mut pending: HashMap<PathBuf, Pending> = HashMap::new();
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
                        pending.insert(old.clone(), Pending::new(ChangeKind::Removed, None));
                        pending.insert(
                            new.clone(),
                            Pending::new(
                                ChangeKind::Renamed,
                                Some(format!("由 {} 重命名而来", old.display())),
                            ),
                        );
                        self.probe(&new, &mut probes, &mut last_probe);
                        quiet_since = Some(Instant::now());
                        continue;
                    }

                    let mut force_flush = false;
                    for path in &event.paths {
                        if pending.len() >= MAX_PENDING {
                            // Back-pressure: do not wait for a quiet window
                            // when the pending map is already full.
                            force_flush = true;
                            break;
                        }
                        let entry = pending
                            .entry(path.clone())
                            .or_insert_with(|| Pending::new(kind, None));
                        // "created" is strictly more informative than a later
                        // "modified" in the same burst, so never downgrade it.
                        if entry.kind == ChangeKind::Modified && kind == ChangeKind::Created {
                            entry.kind = ChangeKind::Created;
                        }
                        if kind != ChangeKind::Removed {
                            self.probe(path, &mut probes, &mut last_probe);
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

            if !pending.is_empty()
                && quiet_since
                    .map(|t| t.elapsed() >= DEBOUNCE)
                    .unwrap_or(false)
            {
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
                let _ = self.store.record_event(
                    id,
                    "removed",
                    now_ms(),
                    0,
                    None,
                    Some("文件已从监控目录中消失"),
                );
            }
            let _ = self.store.mark_gone(&key);
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
        let combined = if notes.is_empty() {
            None
        } else {
            Some(notes.join("；"))
        };

        let src = self.resolve_source(&obs.path, &obs.name, obs.size, probe.as_ref());
        let Ok(id) = self.store.upsert_file(&obs, &src) else {
            return;
        };
        let attributed = probe.as_ref().filter(|a| a.is_usable());
        let hit = attributed.is_some();
        let recorded = self
            .store
            .record_event(
                id,
                kind.as_str(),
                now_ms(),
                obs.size,
                attributed,
                combined.as_deref(),
            )
            .unwrap_or(false);
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
        if self
            .store
            .record_opened(file_id, ts, &note)
            .unwrap_or(false)
        {
            self.update(|s| s.events_this_session += 1);
        }
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
            let _ = self.store.promote_settled(now_ms(), settle_seconds);

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
                self.update(|s| {
                    if s.scan_state == "hashing" {
                        s.scan_state = "ready".into();
                    }
                });
                std::thread::sleep(Duration::from_millis(1_500));
                continue;
            }
            self.update(|s| {
                if s.scan_state != "scanning" {
                    s.scan_state = "hashing".into();
                }
            });

            for (id, path, size) in batch {
                if !self.running.load(Ordering::Relaxed) {
                    return;
                }
                if size < 0 || size as u64 > MAX_HASH_BYTES {
                    self.store.set_hash_state(id, "too_large");
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
                        let _ = self.store.set_digest(
                            id,
                            &digest.root,
                            &digest.chunks,
                            digest.incremental,
                        );
                        window_chunks += chunks.max(1);
                        self.update(|s| s.hashed_count += 1);
                    }
                    Err(HashError::TooLarge) => self.store.set_hash_state(id, "too_large"),
                    Err(HashError::Io(_)) => self.store.set_hash_state(id, "error"),
                }
            }
        }
    }

    // ---------------------------------------------------------- maintenance

    fn maintenance_loop(&self) {
        // Let the initial scan finish before competing for the database.
        for _ in 0..6 {
            if !self.running.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_secs(30));
        }

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
        EventKind::Modify(ModifyKind::Name(_)) => Some(ChangeKind::Modified),
        EventKind::Modify(_) => Some(ChangeKind::Modified),
        EventKind::Access(_) => None,
        _ => Some(ChangeKind::Modified),
    }
}
