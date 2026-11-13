//! Process-level runtime concerns: elevation state, single instance, and the
//! per-user autostart entry.
//!
//! V2 turns the app into a resident background daemon, which means it must
//! never run twice, and it must be able to restart itself after a reboot.

use std::path::Path;
use std::process::Command;

use crate::win32 as w;

const AUTOSTART_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const AUTOSTART_VALUE: &str = "FileLineage";

/// Command-line switch the autostart entry passes so a reboot lands in the tray.
pub const MINIMIZED_FLAG: &str = "--minimized";

/// `Local\` keeps the mutex inside the current logon session, so two different
/// users may each run their own instance.
const MUTEX_NAME: &str = r"Local\FileLineage.SingleInstance.v1";

/// True when the process token is elevated. ETW kernel sessions need this;
/// everything else works without it.
pub fn is_elevated() -> bool {
    unsafe {
        let mut token: w::Handle = std::ptr::null_mut();
        if w::OpenProcessToken(w::GetCurrentProcess(), w::TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = w::TokenElevation { is_elevated: 0 };
        let mut returned: u32 = 0;
        let ok = w::GetTokenInformation(
            token,
            w::TOKEN_ELEVATION_CLASS,
            &mut elevation as *mut _ as *mut core::ffi::c_void,
            std::mem::size_of::<w::TokenElevation>() as u32,
            &mut returned,
        );
        w::CloseHandle(token);
        ok != 0 && elevation.is_elevated != 0
    }
}

/// Owns the single-instance mutex. The handle is intentionally kept for the
/// whole process lifetime; `Drop` releases the name.
pub struct SingleInstance {
    handle: w::Handle,
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe {
            w::CloseHandle(self.handle);
        }
    }
}

/// Returns `None` when another instance already holds the name.
pub fn acquire_single_instance() -> Option<SingleInstance> {
    let name = crate::util::to_wide(std::ffi::OsStr::new(MUTEX_NAME));
    unsafe {
        w::SetLastError(0);
        let handle = w::CreateMutexW(std::ptr::null_mut(), 0, name.as_ptr());
        if handle.is_null() {
            return None;
        }
        if w::GetLastError() == w::ERROR_ALREADY_EXISTS {
            w::CloseHandle(handle);
            return None;
        }
        Some(SingleInstance { handle })
    }
}

fn run_reg(args: &[&str]) -> Option<String> {
    use std::os::windows::process::CommandExt;
    let output = Command::new("reg")
        .args(args)
        .creation_flags(w::CREATE_NO_WINDOW)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Is our autostart entry currently registered?
pub fn autostart_enabled() -> bool {
    run_reg(&["query", AUTOSTART_KEY, "/v", AUTOSTART_VALUE])
        .map(|out| out.contains(AUTOSTART_VALUE))
        .unwrap_or(false)
}

/// Read back the command line we registered, if any.
pub fn autostart_command() -> Option<String> {
    let out = run_reg(&["query", AUTOSTART_KEY, "/v", AUTOSTART_VALUE])?;
    let line = out.lines().find(|l| l.contains(AUTOSTART_VALUE))?;
    let marker = line.find("REG_SZ")?;
    let value = line[marker + "REG_SZ".len()..].trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// Register or remove the per-user autostart entry (`HKCU\...\Run`).
///
/// Writing the key ourselves keeps the app free of an extra Tauri plugin and
/// works identically for the NSIS per-user install layout.
pub fn set_autostart(enable: bool, exe: &Path) -> Result<(), String> {
    if enable {
        let command = format!("\"{}\" {}", exe.to_string_lossy(), MINIMIZED_FLAG);
        run_reg(&[
            "add",
            AUTOSTART_KEY,
            "/v",
            AUTOSTART_VALUE,
            "/t",
            "REG_SZ",
            "/d",
            &command,
            "/f",
        ])
        .map(|_| ())
        .ok_or_else(|| "reg add 失败".to_string())
    } else {
        // A missing value is not an error: the goal state is "not present".
        let _ = run_reg(&["delete", AUTOSTART_KEY, "/v", AUTOSTART_VALUE, "/f"]);
        Ok(())
    }
}

/// Bring the already-running instance's window to the front.
///
/// A second launch holds no IPC channel, so it looks the window up by title.
/// Returns `false` when nothing was found, in which case the caller simply
/// exits quietly — the resident daemon keeps running either way.
pub fn focus_existing_window(title: &str) -> bool {
    unsafe {
        let wide = crate::util::to_wide(std::ffi::OsStr::new(title));
        let window = w::FindWindowW(std::ptr::null(), wide.as_ptr());
        if window.is_null() {
            return false;
        }
        if w::IsIconic(window) != 0 {
            w::ShowWindow(window, w::SW_RESTORE);
        }
        w::ShowWindow(window, w::SW_SHOW);
        w::SetForegroundWindow(window);
        true
    }
}
