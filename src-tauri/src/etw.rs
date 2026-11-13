//! ETW channel: the actual writer PID, from `Microsoft-Windows-Kernel-File`.
//!
//! # Why a manifest kernel provider and not the NT Kernel Logger
//!
//! The classic NT Kernel Logger session (`KERNEL_LOGGER_NAME`) is globally
//! exclusive, so starting it fights with Process Monitor, some antivirus
//! products and WPR; its MOF `FileIo` payloads also fail TDH decoding on
//! current Windows builds. `Microsoft-Windows-Kernel-File` is a manifest
//! provider with named fields and can be enabled with `EnableTraceEx2` in an
//! ordinary user session, where several consumers can coexist.
//!
//! # The non-obvious bit: `Write` has no file name
//!
//! Real provider payloads (verified against a live trace):
//!
//! | task | key fields |
//! | --- | --- |
//! | `NameCreate` (10) | `FileKey`, `FileName` |
//! | `Create` (12) | `FileObject`, `FileName` |
//! | `Write` (16) | `FileObject`, `FileKey`, `IOSize`, … **no path** |
//! | `DeletePath` (26) | `FilePath` |
//! | `Cleanup` / `Close` (13/14) | `FileObject`, `FileKey` |
//!
//! So we keep a bounded `FileObject`/`FileKey → (path, opener PID)` table,
//! populated on create/name events and evicted on cleanup/close. Paths are NT
//! device paths (`\Device\HarddiskVolume3\…`), translated to DOS form before
//! they enter the index. Cached writes flushed by the System process arrive
//! with PID 4, so in that case we prefer the recorded opener PID.
//!
//! # Platform split
//!
//! ETW only exists on Windows. The two `imp` modules below expose the exact
//! same surface (`EtwIndex::new/take/len`, `start`), so `collector.rs` does not
//! need a single `#[cfg]`. On macOS/Linux `start` returns
//! [`EtwStart::Unsupported`] and the index is an empty no-op.
//!
//! # Safety (Windows)
//!
//! The callback runs on ETW's dispatch thread and ferrisetw terminates the
//! process if a callback panics. There is therefore no `unwrap`, `expect`,
//! indexing or allocation-heavy work here — only a couple of bounded maps.

/// Outcome of trying to start the ETW channel.
pub enum EtwStart {
    Active,
    /// Not elevated; the provider cannot be enabled.
    NotElevated,
    #[allow(dead_code)]
    Disabled,
    /// The platform has no ETW at all (macOS / Linux).
    #[allow(dead_code)]
    Unsupported,
    Failed(String),
}

#[cfg(windows)]
mod imp {
    use std::collections::HashMap;
    use std::ffi::OsStr;
    use std::sync::{Arc, Mutex, OnceLock};

    use ferrisetw::parser::{Parser, Pointer};
    use ferrisetw::provider::Provider;
    use ferrisetw::schema_locator::SchemaLocator;
    use ferrisetw::trace::{stop_trace_by_name, UserTrace};
    use ferrisetw::EventRecord;

    use super::EtwStart;
    use crate::attrib::Attribution;
    use crate::util::now_ms;
    use crate::win32 as w;

    /// `Microsoft-Windows-Kernel-File`
    const KERNEL_FILE_PROVIDER: &str = "EDD08927-9CC4-4E65-B970-C2560FB5C289";
    /// Fixed name so a session orphaned by a crash can be cleaned up by name.
    const TRACE_NAME: &str = "FileLineage-FileIO";

    /// Keywords for filename/create/write/delete/rename, deliberately excluding
    /// READ (0x100): read events dominate the volume and are not changes.
    /// 0x10 FILENAME | 0x20 FILEIO | 0x40 OP_END | 0x80 CREATE |
    /// 0x200 WRITE | 0x400 DELETE_PATH | 0x800 RENAME_SETLINK_PATH |
    /// 0x1000 CREATE_NEW_FILE
    const KERNEL_FILE_KEYWORDS: u64 = 0x1EF0;

    const MAX_ENTRIES: usize = 16_384;
    const MAX_OPEN_FILES: usize = 16_384;
    const RETENTION_MS: i64 = 30_000;

    /// Recent `(path → PID)` observations plus the open-file correlation table.
    #[derive(Default)]
    pub struct EtwIndex {
        entries: Mutex<HashMap<String, (u32, i64)>>,
        /// FileObject / FileKey -> (DOS path, PID that opened it)
        open_files: Mutex<HashMap<u64, (String, u32)>>,
    }

    impl EtwIndex {
        pub fn new() -> Arc<Self> {
            Arc::new(Self::default())
        }

        fn record(&self, path: &str, pid: u32, ts: i64) {
            if pid == 0 || pid == 4 || pid == std::process::id() || path.is_empty() {
                return;
            }
            let Ok(mut entries) = self.entries.lock() else {
                return;
            };
            if entries.len() >= MAX_ENTRIES {
                entries.retain(|_, (_, seen)| ts - *seen < RETENTION_MS);
                if entries.len() >= MAX_ENTRIES {
                    entries.clear();
                }
            }
            entries.insert(normalize(path), (pid, ts));
        }

        fn remember_open(&self, keys: &[Option<u64>], path: &str, pid: u32) {
            if path.is_empty() {
                return;
            }
            let Ok(mut open) = self.open_files.lock() else {
                return;
            };
            if open.len() >= MAX_OPEN_FILES {
                open.clear();
            }
            for key in keys.iter().flatten() {
                open.insert(*key, (path.to_string(), pid));
            }
        }

        fn lookup_open(&self, keys: &[Option<u64>]) -> Option<(String, u32)> {
            let open = self.open_files.lock().ok()?;
            for key in keys.iter().flatten() {
                if let Some(found) = open.get(key) {
                    return Some(found.clone());
                }
            }
            None
        }

        fn forget_open(&self, keys: &[Option<u64>]) {
            if let Ok(mut open) = self.open_files.lock() {
                for key in keys.iter().flatten() {
                    open.remove(key);
                }
            }
        }

        /// Consume the observation for a path when it is fresh enough to explain
        /// the change we just saw.
        pub fn take(&self, path: &str, max_age_ms: i64) -> Option<Attribution> {
            let Ok(mut entries) = self.entries.lock() else {
                return None;
            };
            let (pid, ts) = entries.remove(&normalize(path))?;
            if now_ms() - ts > max_age_ms || pid == 0 || pid == 4 || pid == std::process::id() {
                return None;
            }
            Some(Attribution::from_pid(pid, "etw"))
        }

        #[allow(dead_code)]
        pub fn len(&self) -> usize {
            self.entries
                .lock()
                .map(|entries| entries.len())
                .unwrap_or(0)
        }
    }

    fn normalize(path: &str) -> String {
        path.replace('/', "\\").to_lowercase()
    }

    /// Start the ETW channel. Never returns `Active` unless the trace really is
    /// running; every failure path is reported instead of panicking.
    pub fn start(index: Arc<EtwIndex>) -> EtwStart {
        if !crate::runtime::is_elevated() {
            return EtwStart::NotElevated;
        }

        // A previous run may have crashed and left a named session behind.
        let _ = stop_trace_by_name(TRACE_NAME);

        let provider = Provider::by_guid(KERNEL_FILE_PROVIDER)
            .any(KERNEL_FILE_KEYWORDS)
            .level(5)
            .add_callback(
                move |record: &EventRecord, schema_locator: &SchemaLocator| {
                    on_event(&index, record, schema_locator);
                },
            )
            .build();

        match UserTrace::new()
            .named(TRACE_NAME.to_string())
            .enable(provider)
            .start_and_process()
        {
            Ok(trace) => {
                // The trace owns the session: dropping it (or `UserTrace::stop`)
                // closes the session and stops the callbacks. A resident daemon
                // wants it to live for the whole process, so the handle is
                // intentionally leaked rather than stored (which would require
                // `UserTrace: Send`).
                std::mem::forget(trace);
                EtwStart::Active
            }
            Err(error) => {
                // ferrisetw's builder can leave the session behind when
                // EnableTraceEx2 / OpenTraceW fails after StartTraceW succeeded.
                let _ = stop_trace_by_name(TRACE_NAME);
                EtwStart::Failed(format!("{:?}", error))
            }
        }
    }

    /// ETW callback. Must not panic: ferrisetw terminates the process on panic.
    fn on_event(index: &EtwIndex, record: &EventRecord, schema_locator: &SchemaLocator) {
        let Ok(schema) = schema_locator.event_schema(record) else {
            return;
        };
        let task_name = schema.task_name();
        let task = if task_name.is_empty() {
            task_from_event_id(record.event_id())
        } else {
            task_name.as_str()
        };
        if task.is_empty() {
            return;
        }

        let parser = Parser::create(record, &schema);
        // These are pointer-sized fields; parsing them as u32 would fail on x64.
        let file_object = parser
            .try_parse::<Pointer>("FileObject")
            .ok()
            .map(|p| *p as u64);
        let file_key = parser
            .try_parse::<Pointer>("FileKey")
            .ok()
            .map(|p| *p as u64);
        let keys: [Option<u64>; 2] = [file_object, file_key];
        let direct = parser
            .try_parse::<String>("FileName")
            .ok()
            .or_else(|| parser.try_parse::<String>("FilePath").ok());

        let header_pid = record.process_id();
        let mut pid = header_pid;
        let mut path: Option<String> = direct.as_deref().map(dos_path);

        match task {
            "NameCreate" | "Create" | "CreateNewFile" => {
                if let Some(found) = &path {
                    index.remember_open(&keys, found, header_pid);
                }
            }
            "Write" | "SetInformation" | "Read" => {
                // These events have no path. Resolve through the correlation table;
                // for cached writes flushed by System (PID 4), prefer the opener.
                if let Some((found, opener_pid)) = index.lookup_open(&keys) {
                    path = Some(found);
                    if header_pid == 0 || header_pid == 4 {
                        pid = opener_pid;
                    }
                }
                if task == "Read" {
                    return;
                }
            }
            "DeletePath" | "SetDelete" | "NameDelete" | "RenamePath" | "Rename"
            | "SetLinkPath" | "SetLink" => {}
            "Cleanup" | "Close" => {
                index.forget_open(&keys);
                return;
            }
            _ => return,
        }

        if let Some(found) = path {
            index.record(&found, pid, now_ms());
        }
    }

    /// Numeric event id → task name, used only when TDH gives us no `TaskName`.
    fn task_from_event_id(event_id: u16) -> &'static str {
        match event_id {
            10 => "NameCreate",
            11 => "NameDelete",
            12 => "Create",
            13 => "Cleanup",
            14 => "Close",
            15 => "Read",
            16 => "Write",
            17 => "SetInformation",
            18 => "SetDelete",
            19 => "Rename",
            20 => "DirEnum",
            21 => "Flush",
            22 => "QueryInformation",
            23 => "FSCTL",
            24 => "OperationEnd",
            25 => "DirNotify",
            26 => "DeletePath",
            27 => "RenamePath",
            28 => "SetLinkPath",
            29 => "SetLink",
            30 => "CreateNewFile",
            31 => "SetSecurity",
            32 => "QuerySecurity",
            33 => "SetEA",
            _ => "",
        }
    }

    /// Translate a kernel device path (`\Device\HarddiskVolume3\…`) into a DOS path
    /// (`C:\…`). Paths already in DOS form are returned unchanged.
    #[allow(dead_code)]
    pub fn dos_path(path: &str) -> String {
        let stripped = if let Some(rest) = path.strip_prefix(r"\??\") {
            rest
        } else if path.starts_with(r"\Device\") || path.starts_with(r"\SystemRoot") {
            path
        } else {
            return path.to_string();
        };
        if let Some(rest) = stripped.strip_prefix(r"\SystemRoot") {
            if let Ok(root) = std::env::var("SystemRoot") {
                return format!("{}{}", root, rest);
            }
        }
        for (device, drive) in device_map() {
            if let Some(rest) = stripped.strip_prefix(device.as_str()) {
                return format!("{}{}", drive, rest.trim_start_matches('\\'));
            }
        }
        path.to_string()
    }

    /// `\Device\HarddiskVolumeN` → `X:\` for every mounted drive, longest first.
    fn device_map() -> &'static Vec<(String, String)> {
        static MAP: OnceLock<Vec<(String, String)>> = OnceLock::new();
        MAP.get_or_init(|| {
            let mut out = Vec::new();
            let mut buffer = [0u16; 512];
            let length =
                unsafe { w::GetLogicalDriveStringsW(buffer.len() as u32, buffer.as_mut_ptr()) };
            if length == 0 {
                return out;
            }
            let end = (length as usize).min(buffer.len());
            for chunk in buffer[..end].split(|c| *c == 0) {
                if chunk.len() < 2 {
                    continue;
                }
                let drive = String::from_utf16_lossy(chunk);
                let letter: String = drive.chars().take(2).collect();
                if let Some(device) = query_dos_device(&letter) {
                    out.push((device, format!("{}\\", letter)));
                }
            }
            out.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
            out
        })
    }

    fn query_dos_device(name: &str) -> Option<String> {
        let wide = crate::util::to_wide(OsStr::new(name));
        let mut buffer = [0u16; 1024];
        let length =
            unsafe { w::QueryDosDeviceW(wide.as_ptr(), buffer.as_mut_ptr(), buffer.len() as u32) };
        if length == 0 {
            return None;
        }
        let end = (length as usize).min(buffer.len());
        let text = String::from_utf16_lossy(&buffer[..end]);
        let text = text.trim_end_matches('\0').to_string();
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }
}

/// macOS / Linux: no ETW, and no equivalent process-level event source.
#[cfg(not(windows))]
mod imp {
    use std::sync::Arc;

    use super::EtwStart;
    use crate::attrib::Attribution;

    /// Same shape as the Windows index so call sites stay identical; every
    /// lookup misses.
    #[derive(Default)]
    pub struct EtwIndex;

    impl EtwIndex {
        pub fn new() -> Arc<Self> {
            Arc::new(Self)
        }

        pub fn take(&self, _path: &str, _max_age_ms: i64) -> Option<Attribution> {
            None
        }

        #[allow(dead_code)]
        pub fn len(&self) -> usize {
            0
        }
    }

    pub fn start(_index: Arc<EtwIndex>) -> EtwStart {
        EtwStart::Unsupported
    }
}

pub use imp::{start, EtwIndex};
