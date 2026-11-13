//! Windows process attribution.
//!
//! # Why Restart Manager in V1
//!
//! The design doc calls for ETW (`Microsoft-Windows-Kernel-File`) because it is
//! the only source of truth for "which PID issued this write". But a kernel
//! trace session requires elevation, and an app that must be admin on first
//! launch is not a shippable V1. Restart Manager (`rstrtmgr.dll`) solves a
//! closely related problem without elevation: given a path, it returns every
//! process currently holding a handle to that file. A writer almost always
//! holds such a handle while the filesystem events are flowing, so probing on
//! *raw* watcher events (not after debounce) gives us a genuinely useful
//! answer to "谁改过它" for unprivileged users.
//!
//! The `Attribution` type is deliberately process-agnostic: V2 can swap in a
//! real ETW subscriber behind the same interface without touching storage or
//! the UI.

use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct Attribution {
    pub process_name: Option<String>,
    pub process_path: Option<String>,
    pub pid: Option<u32>,
    /// "restart-manager" | "unknown"
    pub method: String,
}

impl Attribution {
    #[allow(dead_code)]
    pub fn label(&self) -> String {
        self.process_name
            .clone()
            .unwrap_or_else(|| "未知进程".to_string())
    }
}

#[cfg(windows)]
#[allow(dead_code, non_snake_case)]
mod ffi {
    use core::ffi::c_void;

    pub type Handle = *mut c_void;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct FileTime {
        pub low: u32,
        pub high: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct RmUniqueProcess {
        pub pid: u32,
        pub start: FileTime,
    }

    /// Mirror of Win32 `RM_PROCESS_INFO`.
    /// CCH_RM_MAX_APP_NAME = 255, CCH_RM_MAX_SVC_NAME = 63, both +1 for NUL.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct RmProcessInfo {
        pub process: RmUniqueProcess,
        pub app_name: [u16; 256],
        pub service_name: [u16; 64],
        pub app_type: i32,
        pub app_status: u32,
        pub ts_session_id: u32,
        pub restartable: i32,
    }

    #[link(name = "rstrtmgr")]
    extern "system" {
        pub fn RmStartSession(session: *mut u32, flags: u32, key: *mut u16) -> u32;
        pub fn RmRegisterResources(
            session: u32,
            file_count: u32,
            files: *const *const u16,
            app_count: u32,
            apps: *const RmUniqueProcess,
            service_count: u32,
            services: *const *const u16,
        ) -> u32;
        pub fn RmGetList(
            session: u32,
            needed: *mut u32,
            count: *mut u32,
            infos: *mut RmProcessInfo,
            reboot_reasons: *mut u32,
        ) -> u32;
        pub fn RmEndSession(session: u32) -> u32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        pub fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
        pub fn QueryFullProcessImageNameW(
            process: Handle,
            flags: u32,
            buffer: *mut u16,
            size: *mut u32,
        ) -> i32;
        pub fn CloseHandle(object: Handle) -> i32;
    }
}

#[cfg(windows)]
struct SessionGuard(u32);

#[cfg(windows)]
impl Drop for SessionGuard {
    fn drop(&mut self) {
        unsafe {
            ffi::RmEndSession(self.0);
        }
    }
}

/// Ask Windows which processes currently hold this file open.
/// Returns an empty vec on any failure — attribution is best-effort by design.
#[cfg(windows)]
pub fn who_holds(path: &Path) -> Vec<Attribution> {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;

    const ERROR_SUCCESS: u32 = 0;
    const ERROR_MORE_DATA: u32 = 234;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;

    unsafe {
        let mut session: u32 = 0;
        // CCH_RM_SESSION_KEY is 32 WCHARs; give it a trailing NUL too.
        let mut key = [0u16; 33];
        if ffi::RmStartSession(&mut session, 0, key.as_mut_ptr()) != ERROR_SUCCESS {
            return Vec::new();
        }
        let _guard = SessionGuard(session);

        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let files = [wide.as_ptr()];
        if ffi::RmRegisterResources(session, 1, files.as_ptr(), 0, ptr::null(), 0, ptr::null())
            != ERROR_SUCCESS
        {
            return Vec::new();
        }

        let mut needed: u32 = 0;
        let mut count: u32 = 0;
        let mut reasons: u32 = 0;
        let first = ffi::RmGetList(
            session,
            &mut needed,
            &mut count,
            ptr::null_mut(),
            &mut reasons,
        );
        if first != ERROR_SUCCESS && first != ERROR_MORE_DATA {
            return Vec::new();
        }
        if needed == 0 {
            return Vec::new();
        }

        let mut buffer: Vec<ffi::RmProcessInfo> = (0..needed)
            .map(|_| std::mem::zeroed::<ffi::RmProcessInfo>())
            .collect();
        count = needed;
        let second = ffi::RmGetList(
            session,
            &mut needed,
            &mut count,
            buffer.as_mut_ptr(),
            &mut reasons,
        );
        if second != ERROR_SUCCESS {
            return Vec::new();
        }
        buffer.truncate((count as usize).min(buffer.len()));

        let mut out = Vec::new();
        for info in buffer {
            let pid = info.process.pid;
            if pid == 0 || pid == 4 {
                continue; // Idle / System are never the author.
            }
            let app_name = wide_to_string(&info.app_name);
            let image = process_image(pid, PROCESS_QUERY_LIMITED_INFORMATION);
            let name = image
                .as_deref()
                .and_then(|p| Path::new(p).file_name())
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
                .or_else(|| {
                    if app_name.is_empty() {
                        None
                    } else {
                        Some(app_name.clone())
                    }
                });
            out.push(Attribution {
                process_name: name,
                process_path: image,
                pid: Some(pid),
                method: "restart-manager".to_string(),
            });
        }
        out
    }
}

#[cfg(not(windows))]
pub fn who_holds(_path: &Path) -> Vec<Attribution> {
    Vec::new()
}

#[cfg(windows)]
fn wide_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end]).trim().to_string()
}

#[cfg(windows)]
fn process_image(pid: u32, access: u32) -> Option<String> {
    unsafe {
        let handle = ffi::OpenProcess(access, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut len = 1024u32;
        let ok = ffi::QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut len);
        ffi::CloseHandle(handle);
        if ok == 0 || len == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..(len as usize).min(1024)]))
    }
}
