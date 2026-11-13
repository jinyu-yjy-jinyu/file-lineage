mod attrib;
mod collector;
mod config;
mod db;
mod etw;
mod fingerprint;
mod identity;
mod im_layout;
mod model;
mod recent;
mod runtime;
mod sources;
mod util;
mod win32;
mod zoneid;

pub use db::{FileObservation, SourceInfo};

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Manager, WindowEvent};

use config::Config;
use model::{FileDetail, FileRow, Status};

/// Must match `app.windows[0].title` in tauri.conf.json: the second-launch
/// hand-off finds the running window by this exact title.
const WINDOW_TITLE: &str = "文件因果溯源台 · FileLineage";

/// Tray icon, decoded at build time (no runtime image feature needed).
const TRAY_ICON: tauri::image::Image<'static> = tauri::include_image!("icons/32x32.png");

pub struct AppState {
    pub store: Arc<db::Store>,
    pub collector: Arc<collector::Collector>,
    pub status: Arc<Mutex<Status>>,
    pub config: Arc<Mutex<Config>>,
    pub config_path: PathBuf,
}

#[tauri::command]
fn get_status(state: tauri::State<'_, AppState>) -> Status {
    let counts = state.store.counts();
    let mut snapshot = state
        .status
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_default();

    snapshot.file_count = counts.files;
    snapshot.event_count = counts.events;
    snapshot.hashed_count = counts.hashed;
    snapshot.pending_hash = counts.pending;
    snapshot.identified_files = counts.identified;
    snapshot.chunk_count = counts.chunks;
    snapshot.incremental_hashes = counts.incremental;

    snapshot.db_path = state.store.db_path.to_string_lossy().to_string();
    snapshot.db_size = state.store.db_size();
    snapshot.total_db_size = state.store.total_size();
    snapshot.archived_events = state.store.archived_event_count();
    snapshot.archive_files = state.store.archive_file_count();
    snapshot.autostart_command = runtime::autostart_command();
    snapshot.config_path = state.config_path.to_string_lossy().to_string();
    snapshot.schema_version = 2;
    if let Ok(config) = state.config.lock() {
        snapshot.autostart = config.autostart;
    }
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
    // Explorer wants a single `/select,<path>` argument.
    let _ = std::process::Command::new("explorer")
        .arg(format!("/select,{}", path))
        .spawn();
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

fn show_main_window<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn build_tray<R: tauri::Runtime>(
    app: &mut tauri::App<R>,
    config_path: PathBuf,
) -> tauri::Result<()> {
    let show_item = MenuItem::with_id(app, "tray_show", "显示主窗口", true, None::<&str>)?;
    let rescan_item = MenuItem::with_id(app, "tray_rescan", "重新扫描", true, None::<&str>)?;
    let autostart_item = CheckMenuItem::with_id(
        app,
        "tray_autostart",
        "开机自启",
        true,
        runtime::autostart_enabled(),
        None::<&str>,
    )?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit_item = MenuItem::with_id(app, "tray_quit", "退出 FileLineage", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &show_item,
            &rescan_item,
            &autostart_item,
            &separator,
            &quit_item,
        ],
    )?;

    let autostart_handle = autostart_item.clone();
    TrayIconBuilder::with_id("main-tray")
        .menu(&menu)
        .tooltip("文件因果溯源台 · FileLineage")
        .icon(TRAY_ICON)
        // Left click shows the window, right click opens the menu.
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id.as_ref() {
            "tray_show" => show_main_window(app),
            "tray_rescan" => {
                app.state::<AppState>().collector.rescan();
            }
            "tray_autostart" => {
                let current = autostart_handle.is_checked().unwrap_or(false);
                let next = !current;
                let applied = std::env::current_exe()
                    .ok()
                    .map(|exe| runtime::set_autostart(next, &exe).is_ok())
                    .unwrap_or(false);
                if !applied {
                    return;
                }
                let _ = autostart_handle.set_checked(next);
                let _ = config::write_bool(&config_path, "autostart", next);
                let state = app.state::<AppState>();
                if let Ok(mut config) = state.config.lock() {
                    config.autostart = next;
                }
                if let Ok(mut status) = state.status.lock() {
                    status.autostart = next;
                };
            }
            "tray_quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

pub fn run() {
    // A second launch must not start a second collector: hand the window over
    // to the resident instance and leave.
    let Some(_instance) = runtime::acquire_single_instance() else {
        runtime::focus_existing_window(WINDOW_TITLE);
        return;
    };

    let base = std::env::var("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    let data_dir = base.join("FileLineage");
    let _ = std::fs::create_dir_all(&data_dir);
    let db_path = data_dir.join("lineage.db");
    let (config, config_path) = Config::load(&data_dir);

    let store = Arc::new(db::Store::open(&db_path).expect("failed to open the lineage database"));
    let dirs = watch_dirs();
    let minimized = std::env::args().any(|arg| arg == runtime::MINIMIZED_FLAG);

    let status = Arc::new(Mutex::new(Status {
        dirs: dirs
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect(),
        db_path: db_path.to_string_lossy().to_string(),
        config_path: config_path.to_string_lossy().to_string(),
        started_at: util::now_ms(),
        scan_state: "idle".to_string(),
        elevated: runtime::is_elevated(),
        etw_state: "starting".to_string(),
        autostart: config.autostart,
        ..Default::default()
    }));

    // Keep the login entry in sync with the configuration.
    if let Ok(exe) = std::env::current_exe() {
        if config.autostart {
            if !runtime::autostart_enabled() {
                let _ = runtime::set_autostart(true, &exe);
            }
        } else if runtime::autostart_enabled() {
            let _ = runtime::set_autostart(false, &exe);
        }
    }

    let minimize_to_tray = config.minimize_to_tray;
    let collector = collector::Collector::new(
        Arc::clone(&store),
        Arc::clone(&status),
        dirs,
        config.clone(),
    );
    let starter = Arc::clone(&collector);
    let config_state = Arc::new(Mutex::new(config));
    let tray_config_path = config_path.clone();

    tauri::Builder::default()
        .manage(AppState {
            store,
            collector,
            status,
            config: Arc::clone(&config_state),
            config_path,
        })
        .setup(move |app| {
            if minimized {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.hide();
                }
            }
            starter.start();
            build_tray(app, tray_config_path.clone())?;
            Ok(())
        })
        .on_window_event(move |window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" && minimize_to_tray {
                    // Resident daemon: closing the window parks it in the tray.
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_status,
            list_files,
            get_file_detail,
            rescan,
            reveal
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(move |_app, event| {
            if let tauri::RunEvent::ExitRequested { code, api, .. } = event {
                // Keep running when the last window closes, but let an explicit
                // `app.exit(0)` (the tray's 退出 item) through. When the user
                // disabled minimize-to-tray, closing the window really exits.
                if code.is_none() && minimize_to_tray {
                    api.prevent_exit();
                }
            }
        });
}
