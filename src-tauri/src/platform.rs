//! Platform-specific runtime concerns.
//!
//! Windows keeps the original Win32 implementation (mutex, `HKCU\...\Run`,
//! Explorer `/select`). macOS and Linux get real equivalents instead of the
//! Windows-only code paths silently returning nothing:
//!
//! | concern | Windows | macOS | Linux |
//! | --- | --- | --- | --- |
//! | data dir | `%LOCALAPPDATA%\FileLineage` | `~/Library/Application Support/FileLineage` | `$XDG_DATA_HOME/FileLineage` |
//! | autostart | `HKCU\...\Run` value | `~/Library/LaunchAgents/*.plist` | `~/.config/autostart/*.desktop` |
//! | single instance | named mutex | `instance.lock` + pid liveness | `instance.lock` + `/proc` liveness |
//! | reveal | Explorer `/select,` | `open -R` | `xdg-open <dir>` |
//! | elevation | process token | always `false` | always `false` |
//!
//! Everything here is deliberately thin: no new Unix-only crate is pulled in.

use std::path::{Path, PathBuf};

/// Command-line switch the autostart entry passes so a reboot lands in the tray.
pub const MINIMIZED_FLAG: &str = "--minimized";

/// Per-user data directory. On Windows this is exactly the previous
/// `%LOCALAPPDATA%\FileLineage`, so an existing install keeps its database.
pub fn data_dir() -> PathBuf {
    dirs::data_local_dir()
        .map(|base| base.join("FileLineage"))
        .unwrap_or_else(|| std::env::temp_dir().join("FileLineage"))
}

fn push_dir(out: &mut Vec<PathBuf>, candidate: Option<PathBuf>) {
    if let Some(path) = candidate {
        if path.is_dir() && !out.contains(&path) {
            out.push(path);
        }
    }
}

/// User folders in the *fingerprint / real-time* scope ("hot dirs"). Uses the
/// platform's real shell folders; the OneDrive-redirected variants only exist
/// on Windows. Everything outside these dirs is index-only (`hash_state=skip`).
pub fn watch_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    push_dir(&mut out, dirs::download_dir());
    push_dir(&mut out, dirs::desktop_dir());
    push_dir(&mut out, dirs::document_dir());

    #[cfg(windows)]
    {
        // OneDrive-redirected folders are common on Chinese Windows installs.
        if let Some(home) = dirs::home_dir() {
            for name in ["OneDrive\\Downloads", "OneDrive\\Desktop", "OneDrive\\文档"] {
                push_dir(&mut out, Some(home.join(name)));
            }
        }
    }
    out
}

// ------------------------------------------------------- full-disk index

/// Enumerate fixed local drives. `GetLogicalDriveStringsW` alone would also
/// return CD-ROM, mapped network and USB drives; those either disappear (watch
/// handle churn) or hold someone else's millions of files, so they are opt-in.
#[cfg(windows)]
fn windows_drive_roots(include_removable: bool, include_network: bool) -> Vec<PathBuf> {
    use crate::win32 as w;
    let mut buffer = [0u16; 512];
    let length = unsafe { w::GetLogicalDriveStringsW(buffer.len() as u32, buffer.as_mut_ptr()) };
    if length == 0 {
        return Vec::new();
    }
    let end = (length as usize).min(buffer.len());
    let mut out = Vec::new();
    for chunk in buffer[..end].split(|c| *c == 0) {
        if chunk.len() < 2 {
            continue;
        }
        // "C:\" — the trailing backslash is required by GetDriveTypeW.
        let root = String::from_utf16_lossy(chunk);
        let wide = crate::util::to_wide(std::ffi::OsStr::new(&root));
        let kind = unsafe { w::GetDriveTypeW(wide.as_ptr()) };
        let include = kind == w::DRIVE_FIXED
            || (include_removable && kind == w::DRIVE_REMOVABLE)
            || (include_network && kind == w::DRIVE_REMOTE);
        if !include {
            continue;
        }
        push_dir(&mut out, Some(PathBuf::from(&root)));
    }
    out
}

/// Roots the background indexer walks. On Windows these are the fixed drives;
/// on Unix it is `/` (prefix exclusions are applied by the walker). If drive
/// enumeration fails we fall back to the hot dirs rather than indexing nothing.
pub fn index_roots(include_removable: bool, include_network: bool) -> Vec<PathBuf> {
    let _ = (include_removable, include_network);
    #[cfg(windows)]
    let roots = windows_drive_roots(include_removable, include_network);
    #[cfg(not(windows))]
    let roots: Vec<PathBuf> = {
        let root = PathBuf::from("/");
        if root.is_dir() {
            vec![root]
        } else {
            Vec::new()
        }
    };
    if roots.is_empty() {
        watch_dirs()
    } else {
        roots
    }
}

/// Root-level prefixes that must never be walked or watched on Unix: virtual
/// filesystems, other mounts and OS-private trees. Directory *names* are
/// excluded separately by `util::should_skip_dir`.
pub fn index_exclusions() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    const PREFIXES: &[&str] = &[
        "/System",
        "/private",
        "/Volumes",
        "/dev",
        "/net",
        "/home",
    ];
    #[cfg(all(unix, not(target_os = "macos")))]
    const PREFIXES: &[&str] = &[
        "/proc",
        "/sys",
        "/dev",
        "/run",
        "/var/run",
        "/tmp",
        "/mnt",
        "/media",
        "/snap",
        "/boot/efi",
    ];
    #[cfg(not(unix))]
    const PREFIXES: &[&str] = &[];
    PREFIXES.iter().map(|prefix| PathBuf::from(*prefix)).collect()
}

/// What the real-time watcher can honestly cover.
///
/// Windows (ReadDirectoryChangesW) and macOS (FSEvents) can watch a whole
/// volume recursively. Linux inotify has a per-user watch limit, so watching
/// `/` silently drops events; there we only watch the hot dirs and let the
/// nightly resweep converge everything else.
pub fn watch_roots(hot_dirs: &[PathBuf], index_roots: &[PathBuf]) -> (Vec<PathBuf>, String) {
    let _ = index_roots;
    #[cfg(windows)]
    let (roots, scope) = (
        if index_roots.is_empty() {
            hot_dirs.to_vec()
        } else {
            index_roots.to_vec()
        },
        "固定磁盘根目录（递归 watch）".to_string(),
    );
    #[cfg(target_os = "macos")]
    let (roots, scope) = (
        if index_roots.is_empty() {
            hot_dirs.to_vec()
        } else {
            index_roots.to_vec()
        },
        "整个磁盘（FSEvents 递归）".to_string(),
    );
    #[cfg(not(any(windows, target_os = "macos")))]
    let (roots, scope) = (
        hot_dirs.to_vec(),
        "仅指纹范围目录（本平台全盘 watch 不可靠，其余依赖每夜重扫）".to_string(),
    );
    (roots, scope)
}

// --------------------------------------------------------------- elevation

/// True when the process token is elevated. ETW kernel sessions need this;
/// everything else works without it. Non-Windows platforms have no ETW at all,
/// so this is always `false`.
#[cfg(windows)]
pub fn is_elevated() -> bool {
    use crate::win32 as w;
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

#[cfg(not(windows))]
pub fn is_elevated() -> bool {
    false
}

// ---------------------------------------------------------- single instance

#[cfg(windows)]
const MUTEX_NAME: &str = r"Local\FileLineage.SingleInstance.v1";

/// Owns the single-instance mutex. The handle is intentionally kept for the
/// whole process lifetime; `Drop` releases the name.
#[cfg(windows)]
pub struct SingleInstance {
    handle: crate::win32::Handle,
}

#[cfg(windows)]
impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe {
            crate::win32::CloseHandle(self.handle);
        }
    }
}

/// Returns `None` when another instance already holds the name.
#[cfg(windows)]
pub fn acquire_single_instance() -> Option<SingleInstance> {
    use crate::win32 as w;
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

/// Unix single instance: a pid file created with `O_EXCL`. A crash leaves a
/// stale file behind, so a *dead* owner's file is reclaimed on the next start.
#[cfg(not(windows))]
pub struct SingleInstance {
    path: PathBuf,
}

#[cfg(not(windows))]
impl Drop for SingleInstance {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(not(windows))]
fn process_alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        Path::new(&format!("/proc/{}", pid)).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        // macOS: no /proc. `ps -p` is the portable liveness probe; if ps itself
        // fails we must assume alive (never steal a live instance's lock).
        std::process::Command::new("ps")
            .arg("-p")
            .arg(pid.to_string())
            .output()
            .map(|output| output.status.success())
            .unwrap_or(true)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        true
    }
}

#[cfg(not(windows))]
pub fn acquire_single_instance() -> Option<SingleInstance> {
    let dir = data_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("instance.lock");
    for _ in 0..2 {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                use std::io::Write;
                let _ = writeln!(file, "{}", std::process::id());
                return Some(SingleInstance { path });
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let stale = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|text| text.trim().parse::<u32>().ok())
                    .map(|pid| !process_alive(pid))
                    .unwrap_or(false);
                if stale {
                    let _ = std::fs::remove_file(&path);
                    continue;
                }
                return None;
            }
            Err(_) => return None,
        }
    }
    None
}

// ---------------------------------------------------------------- autostart

/// `(enabled, command)` cached for 30 s: the status poll runs every 5 s and
/// must not fork a `reg.exe` / read a plist each time.
type AutostartCache = Option<(i64, bool, Option<String>)>;

static AUTOSTART_CACHE: std::sync::Mutex<AutostartCache> = std::sync::Mutex::new(None);
const AUTOSTART_TTL_MS: i64 = 30_000;

fn autostart_state() -> (bool, Option<String>) {
    let now = crate::util::now_ms();
    if let Ok(guard) = AUTOSTART_CACHE.lock() {
        if let Some((ts, enabled, command)) = guard.as_ref() {
            if now - *ts < AUTOSTART_TTL_MS {
                return (*enabled, command.clone());
            }
        }
    }
    let state = read_autostart();
    if let Ok(mut guard) = AUTOSTART_CACHE.lock() {
        *guard = Some((now, state.0, state.1.clone()));
    }
    state
}

fn invalidate_autostart_cache() {
    if let Ok(mut guard) = AUTOSTART_CACHE.lock() {
        *guard = None;
    }
}

/// Is our autostart entry currently registered?
pub fn autostart_enabled() -> bool {
    autostart_state().0
}

/// Read back the command line we registered, if any.
pub fn autostart_command() -> Option<String> {
    autostart_state().1
}

/// Register or remove the per-user autostart entry. The read-back cache is
/// invalidated so the tray checkbox and the status endpoint agree immediately.
pub fn set_autostart(enable: bool, exe: &Path) -> Result<(), String> {
    let result = write_autostart(enable, exe);
    invalidate_autostart_cache();
    result
}

#[cfg(windows)]
const AUTOSTART_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const AUTOSTART_VALUE: &str = "FileLineage";

#[cfg(windows)]
fn run_reg(args: &[&str]) -> Option<String> {
    use std::os::windows::process::CommandExt;
    let output = std::process::Command::new("reg")
        .args(args)
        .creation_flags(crate::win32::CREATE_NO_WINDOW)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(windows)]
fn read_autostart() -> (bool, Option<String>) {
    let Some(out) = run_reg(&["query", AUTOSTART_KEY, "/v", AUTOSTART_VALUE]) else {
        return (false, None);
    };
    let Some(line) = out.lines().find(|l| l.contains(AUTOSTART_VALUE)) else {
        return (false, None);
    };
    let command = line
        .find("REG_SZ")
        .map(|marker| line[marker + "REG_SZ".len()..].trim().to_string())
        .filter(|value| !value.is_empty());
    (true, command)
}

/// Writing the key ourselves keeps the app free of an extra Tauri plugin and
/// works identically for the NSIS per-user install layout.
#[cfg(windows)]
fn write_autostart(enable: bool, exe: &Path) -> Result<(), String> {
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

#[cfg(target_os = "macos")]
fn autostart_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Library")
        .join("LaunchAgents")
        .join("io.github.filelineage.desktop.plist")
}

#[cfg(target_os = "macos")]
fn read_autostart() -> (bool, Option<String>) {
    let path = autostart_path();
    if !path.is_file() {
        return (false, None);
    }
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let command = text
        .split("<string>")
        .nth(1)
        .and_then(|rest| rest.split("</string>").next())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    (true, command)
}

#[cfg(target_os = "macos")]
fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(target_os = "macos")]
fn write_autostart(enable: bool, exe: &Path) -> Result<(), String> {
    let path = autostart_path();
    if !enable {
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let body = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>io.github.filelineage.desktop</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
        <string>{}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>ProcessType</key>
    <string>Background</string>
</dict>
</plist>
"#,
        xml_escape(&exe.to_string_lossy()),
        MINIMIZED_FLAG
    );
    std::fs::write(&path, body).map_err(|e| e.to_string())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn autostart_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("autostart")
        .join("file-lineage.desktop")
}

#[cfg(all(unix, not(target_os = "macos")))]
fn read_autostart() -> (bool, Option<String>) {
    let path = autostart_path();
    if !path.is_file() {
        return (false, None);
    }
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let command = text
        .lines()
        .find_map(|line| line.strip_prefix("Exec="))
        .map(|value| value.trim().trim_matches('"').to_string())
        .filter(|value| !value.is_empty());
    (true, command)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn write_autostart(enable: bool, exe: &Path) -> Result<(), String> {
    let path = autostart_path();
    if !enable {
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let body = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=FileLineage\n\
         Comment=文件因果溯源台\n\
         Exec=\"{}\" {}\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n",
        exe.to_string_lossy(),
        MINIMIZED_FLAG
    );
    std::fs::write(&path, body).map_err(|e| e.to_string())
}

// ------------------------------------------------------------------- reveal

/// Show a file in the platform file manager.
///
/// Windows: Explorer requires the quotes to wrap *only* the path, which is why
/// this uses `raw_arg` instead of `arg` (Rust's own escaping would quote the
/// whole `/select,…` argument and the file would not be selected).
pub fn reveal(path: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let trimmed = path.trim_end_matches('\\');
        std::process::Command::new("explorer")
            .raw_arg(format!("/select,\"{}\"", trimmed))
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // xdg-open has no "select this file" mode; opening the containing
        // directory is the honest equivalent.
        let target = Path::new(path)
            .parent()
            .map(|parent| parent.to_path_buf())
            .unwrap_or_else(|| PathBuf::from(path));
        std::process::Command::new("xdg-open")
            .arg(&target)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = path;
        Err("本平台不支持在文件管理器中显示".to_string())
    }
}

// ----------------------------------------------------- focus existing window

/// Bring the already-running instance's window to the front.
///
/// Windows looks the window up by title. Other platforms have no cheap
/// title-based lookup without an extra plugin, so this degrades to `false`:
/// the caller exits quietly and the resident daemon keeps running.
#[cfg(windows)]
pub fn focus_existing_window(title: &str) -> bool {
    use crate::win32 as w;
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

#[cfg(not(windows))]
pub fn focus_existing_window(_title: &str) -> bool {
    false
}
