mod attrib;
mod collector;
mod db;
mod fingerprint;
mod model;
mod sources;
mod util;

pub use db::{FileObservation, SourceInfo};

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use model::{FileDetail, FileRow, Status};

pub struct AppState {
    pub store: Arc<db::Store>,
    pub collector: Arc<collector::Collector>,
    pub status: Arc<Mutex<Status>>,
}

#[tauri::command]
fn get_status(state: tauri::State<'_, AppState>) -> Status {
    let (files, events, hashed, pending) = state.store.counts();
    let mut snapshot = state
        .status
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_default();
    snapshot.file_count = files;
    snapshot.event_count = events;
    snapshot.hashed_count = hashed;
    snapshot.pending_hash = pending;
    snapshot.db_path = state.store.db_path.to_string_lossy().to_string();
    snapshot.db_size = state.store.db_size();
    snapshot
}

#[tauri::command]
fn list_files(
    query: String,
    limit: i64,
    offset: i64,
    state: tauri::State<'_, AppState>,
) -> Vec<FileRow> {
    state
        .store
        .list_files(&query, limit.clamp(1, 500), offset.max(0))
}

#[tauri::command]
fn get_file_detail(id: i64, state: tauri::State<'_, AppState>) -> Option<FileDetail> {
    state.store.file_detail(id)
}

#[tauri::command]
fn rescan(state: tauri::State<'_, AppState>) {
    state.collector.rescan();
}

#[tauri::command]
fn reveal(path: String) {
    #[cfg(windows)]
    {
        // Explorer wants a single `/select,<path>` argument.
        let _ = std::process::Command::new("explorer")
            .arg(format!("/select,{}", path))
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = path;
    }
}

fn watch_dirs() -> Vec<PathBuf> {
    let home = std::env::var("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    let mut dirs = Vec::new();
    for name in ["Downloads", "Desktop", "Documents"] {
        let candidate = home.join(name);
        if candidate.is_dir() {
            dirs.push(candidate);
        }
    }
    // OneDrive-redirected folders are common on Chinese Windows installs.
    for name in ["OneDrive\\Downloads", "OneDrive\\Desktop", "OneDrive\\文档"] {
        let candidate = home.join(name);
        if candidate.is_dir() {
            dirs.push(candidate);
        }
    }
    dirs
}

pub fn run() {
    let base = std::env::var("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    let db_path = base.join("FileLineage").join("lineage.db");
    let store = Arc::new(db::Store::open(&db_path).expect("failed to open the lineage database"));

    let dirs = watch_dirs();
    let status = Arc::new(Mutex::new(Status {
        dirs: dirs
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect(),
        db_path: db_path.to_string_lossy().to_string(),
        started_at: util::now_ms(),
        scan_state: "idle".to_string(),
        ..Default::default()
    }));

    let collector = collector::Collector::new(Arc::clone(&store), Arc::clone(&status), dirs);
    let starter = Arc::clone(&collector);

    tauri::Builder::default()
        .manage(AppState {
            store,
            collector,
            status,
        })
        .setup(move |_app| {
            starter.start();
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_status,
            list_files,
            get_file_detail,
            rescan,
            reveal
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
