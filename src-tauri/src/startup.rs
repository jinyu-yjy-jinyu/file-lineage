//! Startup diagnostics.
//!
//! A release Windows GUI app (`windows_subsystem = "windows"`) has no console,
//! so a panic or an early `return` looks exactly like "double-clicking does
//! nothing". V2.0.0 shipped a database-migration bug that did precisely that.
//!
//! This module makes every startup outcome observable:
//!   * a small append-only `startup.log` next to the database;
//!   * a panic hook that writes the panic payload there before delegating to
//!     the default hook;
//!   * `fatal()` which logs AND shows a modal error box, used for the two
//!     fallible startup steps (database open, Tauri app build).
//!
//! Nothing here may panic: `log` is called from inside the panic hook.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::win32 as w;

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Rotate the log when it grows past 512 KiB so a repeatedly failing install
/// cannot fill the directory.
const MAX_LOG_BYTES: u64 = 512 * 1024;

pub fn init(data_dir: &Path) {
    let path = data_dir.join("startup.log");
    let _ = LOG_PATH.set(path.clone());
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > MAX_LOG_BYTES {
            let _ = std::fs::remove_file(&path);
        }
    }
    log("=== startup begin ===");
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log(&format!("PANIC: {}", info));
        previous(info);
    }));
}

/// Append one timestamped line. Every failure is deliberately ignored.
pub fn log(message: &str) {
    let Some(path) = LOG_PATH.get() else {
        return;
    };
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let _ = writeln!(file, "{}  {}", crate::util::now_ms(), message);
}

/// Show a modal error and log it. Used when startup cannot continue.
pub fn fatal(title: &str, message: &str) {
    log(&format!(
        "FATAL: {} — {}",
        title,
        message.replace('\n', " | ")
    ));
    unsafe {
        let caption = crate::util::to_wide(std::ffi::OsStr::new(title));
        let text = crate::util::to_wide(std::ffi::OsStr::new(message));
        w::MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            caption.as_ptr(),
            w::MB_OK | w::MB_ICONERROR | w::MB_SYSTEMMODAL,
        );
    }
}
