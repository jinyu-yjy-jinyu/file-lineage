//! Windows process attribution — two channels.
//!
//! ## Channel A: ETW kernel trace (`etw.rs`)
//!
//! `Microsoft-Windows-Kernel-File` reports the PID that issued every file
//! operation. It is the ground truth the design doc asks for, and it requires
//! an elevated process. It lives in its own module because it is strictly
//! optional: the app degrades to channel B when not elevated.
//!
//! ## Channel B: Restart Manager (`who_holds`)
//!
//! Given a path, `RmGetList` returns every process currently holding a handle
//! to that file. This needs no elevation at all. A writer almost always holds
//! such a handle while filesystem events are flowing, so probing on *raw*
//! watcher events (not after debounce, and repeatedly across a long write)
//! gives a genuinely useful answer for unprivileged users.
//!
//! Both channels produce the same `Attribution`, so storage and UI never need
//! to know which one answered.

use std::path::Path;

use crate::win32 as w;

#[derive(Debug, Clone, Default)]
pub struct Attribution {
    pub process_name: Option<String>,
    pub process_path: Option<String>,
    pub pid: Option<u32>,
    /// "etw" | "restart-manager" | "unknown"
    pub method: String,
}

impl Attribution {
    pub fn from_pid(pid: u32, method: &str) -> Self {
        let image = process_image(pid);
        let name = image
            .as_deref()
            .and_then(|p| Path::new(p).file_name())
            .and_then(|n| n.to_str())
            .map(|s| s.to_string());
        Self {
            process_name: name,
            process_path: image,
            pid: Some(pid),
            method: method.to_string(),
        }
    }

    /// A usable answer needs at least a PID.
    pub fn is_usable(&self) -> bool {
        self.pid.is_some()
    }
}

struct SessionGuard(u32);

impl Drop for SessionGuard {
    fn drop(&mut self) {
        unsafe {
            w::RmEndSession(self.0);
        }
    }
}

/// Ask Windows which processes currently hold this file open.
/// Returns an empty vec on any failure — attribution is best-effort by design.
pub fn who_holds(path: &Path) -> Vec<Attribution> {
    use std::ptr;

    unsafe {
        let mut session: u32 = 0;
        // CCH_RM_SESSION_KEY is 32 WCHARs; give it a trailing NUL too.
        let mut key = [0u16; 33];
        if w::RmStartSession(&mut session, 0, key.as_mut_ptr()) != w::ERROR_SUCCESS {
            return Vec::new();
        }
        let _guard = SessionGuard(session);

        let wide = crate::util::to_wide(path.as_os_str());
        let files = [wide.as_ptr()];
        if w::RmRegisterResources(session, 1, files.as_ptr(), 0, ptr::null(), 0, ptr::null())
            != w::ERROR_SUCCESS
        {
            return Vec::new();
        }

        let mut needed: u32 = 0;
        let mut count: u32 = 0;
        let mut reasons: u32 = 0;
        let first = w::RmGetList(
            session,
            &mut needed,
            &mut count,
            ptr::null_mut(),
            &mut reasons,
        );
        if first != w::ERROR_SUCCESS && first != w::ERROR_MORE_DATA {
            return Vec::new();
        }
        if needed == 0 {
            return Vec::new();
        }

        let mut buffer: Vec<w::RmProcessInfo> = (0..needed)
            .map(|_| std::mem::zeroed::<w::RmProcessInfo>())
            .collect();
        count = needed;
        let second = w::RmGetList(
            session,
            &mut needed,
            &mut count,
            buffer.as_mut_ptr(),
            &mut reasons,
        );
        if second != w::ERROR_SUCCESS {
            return Vec::new();
        }
        buffer.truncate((count as usize).min(buffer.len()));

        let mut out = Vec::new();
        for info in buffer {
            let pid = info.process.pid;
            if pid == 0 || pid == 4 {
                continue; // Idle / System are never the author.
            }
            let mut attribution = Attribution::from_pid(pid, "restart-manager");
            if attribution.process_name.is_none() {
                // Restart Manager knows a friendly app name even when we cannot
                // open the process (other session, protected process, …).
                let app_name = wide_to_string(&info.app_name);
                if !app_name.is_empty() {
                    attribution.process_name = Some(app_name);
                }
            }
            out.push(attribution);
        }
        out
    }
}

/// Full image path of a PID, or `None` when it cannot be queried.
pub fn process_image(pid: u32) -> Option<String> {
    unsafe {
        let handle = w::OpenProcess(w::PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buffer = [0u16; 1024];
        let mut length = 1024u32;
        let ok = w::QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut length);
        w::CloseHandle(handle);
        if ok == 0 || length == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(
            &buffer[..(length as usize).min(1024)],
        ))
    }
}

fn wide_to_string(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end]).trim().to_string()
}
