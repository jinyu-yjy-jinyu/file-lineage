use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use notify::event::{ModifyKind, RenameMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use walkdir::WalkDir;

use crate::attrib::{who_holds, Attribution};
use crate::db::{FileObservation, SourceInfo, Store};
use crate::fingerprint::{hash_file, HashError, MAX_HASH_BYTES};
use crate::model::Status;
use crate::sources::{classify_path, refine_with_process, Sources};
use crate::util::{is_noise_file, now_ms, should_skip_dir};

const SCAN_BATCH: usize = 400;
const HASH_BATCH: i64 = 32;
const DEBOUNCE: Duration = Duration::from_millis(400);
const PROBE_INTERVAL: Duration = Duration::from_millis(250);
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
    running: AtomicBool,
    started: AtomicBool,
}

impl Collector {
    pub fn new(store: Arc<Store>, status: Arc<Mutex<Status>>, dirs: Vec<PathBuf>) -> Arc<Self> {
        Arc::new(Self {
            store,
            status,
            dirs,
            sources: Mutex::new(Sources::default()),
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

        self.rescan();
    }

    pub fn rescan(self: &Arc<Self>) {
        let me = Arc::clone(self);
        std::thread::Builder::new()
            .name("fl-scanner".into())
            .spawn(move || {
                me.reload_sources();
                me.scan_once();
            })
            .ok();
    }

    #[allow(dead_code)]
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    fn update<F: FnOnce(&mut Status)>(&self, f: F) {
        if let Ok(mut s) = self.status.lock() {
            f(&mut s);
        }
    }

    fn reload_sources(&self) {
        let loaded = Sources::load();
        let (adapters, records) = (
            loaded.adapter_names().to_vec(),
            loaded.record_count() as i64,
        );
        if let Ok(mut guard) = self.sources.lock() {
            *guard = loaded;
        }
        self.update(|s| {
            s.adapters = adapters.clone();
            s.source_records = records;
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
            Ok(guard) => guard
                .lookup(path, name, size)
                .unwrap_or_else(|| classify_path(path)),
            Err(_) => classify_path(path),
        };
        if let Some(process) = probe.and_then(|a| a.process_name.as_deref()) {
            info = refine_with_process(info, process);
        }
        info
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
                    // sides is how V1 preserves short-term file identity
                    // without relying on platform-specific inode APIs.
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

                    for path in &event.paths {
                        if pending.len() >= MAX_PENDING {
                            quiet_since = Some(Instant::now() - DEBOUNCE);
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
                    quiet_since = Some(Instant::now());
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
                // Do not let probe caches grow forever across a long session.
                if last_probe.len() > 4_096 {
                    last_probe.clear();
                    probes.clear();
                }
            }
        }

        drop(watcher);
        self.update(|s| s.watching = false);
    }

    /// Restart Manager is only useful *while* a writer still holds the handle,
    /// so we probe on raw events (before the debounce flush), rate-limited.
    fn probe(
        &self,
        path: &Path,
        probes: &mut HashMap<PathBuf, Attribution>,
        last_probe: &mut HashMap<PathBuf, Instant>,
    ) {
        let too_soon = last_probe
            .get(path)
            .map(|t| t.elapsed() < PROBE_INTERVAL)
            .unwrap_or(false);
        if too_soon || probes.contains_key(path) {
            return;
        }
        last_probe.insert(path.to_path_buf(), Instant::now());
        if let Some(hit) = who_holds(path).into_iter().next() {
            probes.insert(path.to_path_buf(), hit);
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
        let src = self.resolve_source(&obs.path, &obs.name, obs.size, probe.as_ref());
        let Ok(id) = self.store.upsert_file(&obs, &src) else {
            return;
        };
        let hit = probe
            .as_ref()
            .map(|a| a.process_name.is_some())
            .unwrap_or(false);
        let recorded = self
            .store
            .record_event(id, kind.as_str(), now_ms(), obs.size, probe.as_ref(), note)
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
    }

    // -------------------------------------------------------------- hashing

    fn hash_loop(&self) {
        while self.running.load(Ordering::Relaxed) {
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
                match hash_file(Path::new(&path)) {
                    Ok(hash) => {
                        self.store.set_hash(id, &hash);
                        self.update(|s| s.hashed_count += 1);
                    }
                    Err(HashError::TooLarge) => self.store.set_hash_state(id, "too_large"),
                    Err(HashError::Io(_)) => self.store.set_hash_state(id, "error"),
                }
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
