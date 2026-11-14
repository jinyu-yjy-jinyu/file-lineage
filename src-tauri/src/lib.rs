mod attrib;
mod collector;
mod config;
mod db;
mod downloaders;
mod etw;
mod fingerprint;
mod identity;
mod im_layout;
mod model;
mod platform;
mod recent;
mod runtime;
mod sources;
mod startup;
mod util;
mod verdict;
#[cfg(windows)]
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
    // The status endpoint is polled continuously; `stats()` caches the
    // expensive aggregates (counts, archive sizes) for a few seconds instead of
    // re-running seven subqueries — including `COUNT(*) FROM chunks` — on every
    // poll.
    let stats = state.store.stats(5_000);
    let mut snapshot = state
        .status
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_default();

    snapshot.file_count = stats.counts.files;
    snapshot.event_count = stats.counts.events;
    snapshot.hashed_count = stats.counts.hashed;
    snapshot.pending_hash = stats.counts.pending;
    snapshot.identified_files = stats.counts.identified;
    snapshot.chunk_count = stats.counts.chunks;
    snapshot.incremental_hashes = stats.counts.incremental;

    snapshot.db_path = state.store.db_path.to_string_lossy().to_string();
    snapshot.db_size = stats.db_size;
    snapshot.total_db_size = stats.total_db_size;
    snapshot.archived_events = stats.archived_events;
    snapshot.archive_files = stats.archive_files;
    snapshot.autostart_command = runtime::autostart_command();
    snapshot.config_path = state.config_path.to_string_lossy().to_string();
    snapshot.schema_version = state.store.schema_version();
    if let Ok(config) = state.config.lock() {
        snapshot.autostart_configured = config.autostart;
    }
    // H6: the OS entry is the truth, not the config file. Reporting the config
    // value made the UI announce "已开机自启" while the registry had no entry.
    snapshot.autostart = runtime::autostart_enabled();
    snapshot.fts_enabled = state.store.fts_enabled();
    if let Some(error) = state.store.last_error() {
        snapshot.last_error = Some(error);
    }
    snapshot
}

#[tauri::command]
fn list_files(
    query: String,
    kind: String,
    limit: i64,
    offset: i64,
    state: tauri::State<'_, AppState>,
) -> Vec<FileRow> {
    state
        .store
        .list_files(&query, &kind, limit.clamp(1, 500), offset.max(0))
}

/// Total row count for the current filter, so the header can show "X / Y".
#[tauri::command]
fn count_files(query: String, kind: String, state: tauri::State<'_, AppState>) -> i64 {
    state.store.count_files(&query, &kind)
}

#[tauri::command]
fn get_file_detail(id: i64, state: tauri::State<'_, AppState>) -> Option<FileDetail> {
    // The verdict needs to know whether ETW actually answered this session:
    // "ETW configured but not elevated" is a caveat, not a free downgrade.
    let etw_available = state
        .status
        .lock()
        .map(|status| status.etw_state == "active")
        .unwrap_or(false);
    let ctx = crate::verdict::VerdictContext {
        etw_available,
        platform_supports_etw: cfg!(windows),
    };
    state.store.file_detail(id, &ctx)
}

#[tauri::command]
fn rescan(state: tauri::State<'_, AppState>) {
    state.collector.rescan();
}

#[tauri::command]
fn reveal(path: String) -> Result<(), String> {
    platform::reveal(&path)
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
                let exe = std::env::current_exe().ok();
                let applied = match exe.as_deref() {
                    Some(path) => runtime::set_autostart(next, path),
                    None => Err("无法获取当前可执行文件路径".to_string()),
                };
                if let Err(error) = applied {
                    // The OS entry did not change: keep the checkbox where it
                    // was and surface the reason.
                    let message = format!("开机自启切换失败：{}", error);
                    startup::log(&message);
                    let state = app.state::<AppState>();
                    if let Ok(mut status) = state.status.lock() {
                        status.last_error = Some(message);
                    }
                    return;
                }
                let _ = autostart_handle.set_checked(next);
                if let Err(error) = config::write_bool(&config_path, "autostart", next) {
                    // Roll the OS entry back so the checkbox never claims a
                    // state that was not persisted.
                    if let Some(path) = exe.as_deref() {
                        let _ = runtime::set_autostart(current, path);
                    }
                    let _ = autostart_handle.set_checked(current);
                    let message = format!("开机自启配置写入失败：{}", error);
                    startup::log(&message);
                    let state = app.state::<AppState>();
                    if let Ok(mut status) = state.status.lock() {
                        status.last_error = Some(message);
                    }
                    return;
                }
                let state = app.state::<AppState>();
                if let Ok(mut config) = state.config.lock() {
                    config.autostart = next;
                }
                if let Ok(mut status) = state.status.lock() {
                    status.autostart = next;
                    status.autostart_configured = next;
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
    // Diagnostics are installed before anything that can fail. A release GUI
    // binary has no console, so without startup.log a failure is invisible.
    //
    // `platform::data_dir()` is `%LOCALAPPDATA%\FileLineage` on Windows (exactly
    // the old path, so existing installs keep their database) and the platform's
    // local app-data root everywhere else. The old `temp_dir()` fallback put the
    // database in a world-readable, reboot-cleared `/tmp/FileLineage` on macOS
    // and Linux.
    let data_dir = platform::data_dir();
    if let Err(error) = std::fs::create_dir_all(&data_dir) {
        // `fatal` logs when it can, but always reports — worth calling even
        // before `startup::init` has installed the log path.
        startup::fatal(
            "FileLineage 启动失败",
            &format!("无法创建数据目录：{}\n\n{}", data_dir.display(), error),
        );
        return;
    }
    startup::init(&data_dir);

    // A second launch must not start a second collector: hand the window over
    // to the resident instance and leave.
    let Some(_instance) = runtime::acquire_single_instance() else {
        startup::log("second instance detected -> handing off to resident window");
        runtime::focus_existing_window(WINDOW_TITLE);
        return;
    };

    let db_path = data_dir.join("lineage.db");
    let (config, config_path) = Config::load(&data_dir);
    startup::log("config loaded");

    let store = match db::Store::open(&db_path) {
        Ok(store) => {
            startup::log("database opened / migration ok");
            Arc::new(store)
        }
        Err(error) => {
            startup::fatal(
                "FileLineage 启动失败",
                &format!(
                    "无法打开本地数据库（或升级旧版本数据）。\n\n{}\n\n数据文件：{}\n诊断日志：{}",
                    error,
                    db_path.display(),
                    data_dir.join("startup.log").display()
                ),
            );
            return;
        }
    };
    // `dirs` is the fingerprint / real-time scope; `index_roots` is everything
    // the background indexer walks. They are different sets on purpose.
    let dirs = platform::watch_dirs();
    let index_roots = platform::index_roots(config.index_removable, config.index_network);
    store.set_watch_dirs(dirs.clone());
    let (_, watch_scope) = platform::watch_roots(&dirs, &index_roots);
    let minimized = std::env::args().any(|arg| arg == runtime::MINIMIZED_FLAG);

    let status = Arc::new(Mutex::new(Status {
        dirs: dirs
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect(),
        hot_dirs: dirs
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect(),
        index_roots: index_roots
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect(),
        watch_scope,
        fts_enabled: store.fts_enabled(),
        index_state: "idle".to_string(),
        scan_truncated: false,
        db_path: db_path.to_string_lossy().to_string(),
        config_path: config_path.to_string_lossy().to_string(),
        started_at: util::now_ms(),
        scan_state: "idle".to_string(),
        elevated: runtime::is_elevated(),
        etw_state: "starting".to_string(),
        autostart: runtime::autostart_enabled(),
        autostart_configured: config.autostart,
        ..Default::default()
    }));

    // Keep the login entry in sync with the configuration. A failure is a real
    // user-visible problem ("配置说开了，其实没开"), so it is written to both the
    // status and startup.log instead of being dropped.
    if let Ok(exe) = std::env::current_exe() {
        let desired = config.autostart;
        let current = runtime::autostart_enabled();
        if desired != current {
            if let Err(error) = runtime::set_autostart(desired, &exe) {
                let message = format!(
                    "开机自启与配置不一致且同步失败（配置 {}，注册表 {}）：{}",
                    desired, current, error
                );
                startup::log(&message);
                if let Ok(mut guard) = status.lock() {
                    guard.last_error = Some(message);
                }
            }
        }
    }

    let minimize_to_tray = config.minimize_to_tray;
    let collector = collector::Collector::new(
        Arc::clone(&store),
        Arc::clone(&status),
        dirs,
        index_roots,
        config.clone(),
    );
    let starter = Arc::clone(&collector);
    let config_state = Arc::new(Mutex::new(config));
    let tray_config_path = config_path.clone();
    let tray_log_path = data_dir.join("startup.log");

    let app = tauri::Builder::default()
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
            // Tauri 2 runs `setup` on the Ready event inside `App::run`, NOT
            // inside `Builder::build`. A setup error becomes a main-thread
            // panic, so `Builder::build`'s Err arm cannot catch it — surface
            // it here before returning.
            if let Err(error) = build_tray(app, tray_config_path.clone()) {
                startup::fatal(
                    "FileLineage 启动失败",
                    &format!(
                        "无法创建托盘图标或菜单。\n\n{}\n\n诊断日志：{}",
                        error,
                        tray_log_path.display()
                    ),
                );
                return Err(error.into());
            }
            startup::log("setup ok / tray built / collector started");
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
            count_files,
            get_file_detail,
            rescan,
            reveal
        ])
        .build(tauri::generate_context!());

    let app = match app {
        Ok(app) => {
            startup::log("tauri app built / entering event loop");
            app
        }
        Err(error) => {
            startup::fatal(
                "FileLineage 启动失败",
                &format!(
                    "无法创建应用窗口或托盘。\n\n{}\n\n诊断日志：{}",
                    error,
                    data_dir.join("startup.log").display()
                ),
            );
            return;
        }
    };

    app.run(move |_app, event| {
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
