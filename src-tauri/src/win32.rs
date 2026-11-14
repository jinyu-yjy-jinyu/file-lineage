//! Hand-written raw Win32 FFI.
//!
//! We declare these ourselves instead of depending on a specific
//! `windows`/`windows-sys` version: the signatures below are frozen ABI and
//! never drift, whereas the safe wrappers around them do. Everything here is
//! `unsafe` by nature; the safe helpers live in the sibling modules.

#![allow(non_snake_case, dead_code)]

use core::ffi::c_void;

pub type Handle = *mut c_void;
pub type Bool = i32;

pub const INVALID_HANDLE_VALUE: Handle = -1isize as Handle;

pub const ERROR_SUCCESS: u32 = 0;
pub const ERROR_MORE_DATA: u32 = 234;
pub const ERROR_ALREADY_EXISTS: u32 = 183;

pub const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;

pub const FILE_SHARE_READ: u32 = 0x0000_0001;
pub const FILE_SHARE_WRITE: u32 = 0x0000_0002;
pub const FILE_SHARE_DELETE: u32 = 0x0000_0004;
pub const OPEN_EXISTING: u32 = 3;
pub const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// `GetDriveTypeW` return values. Only fixed disks are indexed by default:
/// removable/network drives vanish (watch churn) or hold millions of files.
pub const DRIVE_REMOVABLE: u32 = 2;
pub const DRIVE_FIXED: u32 = 3;
pub const DRIVE_REMOTE: u32 = 4;
pub const DRIVE_CDROM: u32 = 5;
pub const DRIVE_RAMDISK: u32 = 6;

pub const SW_SHOW: i32 = 5;
pub const SW_RESTORE: i32 = 9;

pub const MB_OK: u32 = 0x0;
pub const MB_ICONERROR: u32 = 0x10;
pub const MB_SYSTEMMODAL: u32 = 0x1000;

/// `TOKEN_INFORMATION_CLASS::TokenElevation`
pub const TOKEN_ELEVATION_CLASS: u32 = 20;
pub const TOKEN_QUERY: u32 = 0x0008;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct TokenElevation {
    pub is_elevated: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FileTime {
    pub low: u32,
    pub high: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ByHandleFileInformation {
    pub attributes: u32,
    pub creation: FileTime,
    pub last_access: FileTime,
    pub last_write: FileTime,
    pub volume_serial: u32,
    pub size_high: u32,
    pub size_low: u32,
    pub links: u32,
    pub index_high: u32,
    pub index_low: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RmUniqueProcess {
    pub pid: u32,
    pub start: FileTime,
}

/// Mirror of Win32 `RM_PROCESS_INFO` (668 bytes, 4-byte aligned).
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

#[link(name = "kernel32")]
extern "system" {
    pub fn CreateFileW(
        file_name: *const u16,
        desired_access: u32,
        share_mode: u32,
        security_attributes: *mut c_void,
        creation_disposition: u32,
        flags_and_attributes: u32,
        template_file: Handle,
    ) -> Handle;

    pub fn GetFileInformationByHandle(handle: Handle, info: *mut ByHandleFileInformation) -> Bool;

    pub fn OpenProcess(desired_access: u32, inherit_handle: Bool, pid: u32) -> Handle;

    pub fn QueryFullProcessImageNameW(
        process: Handle,
        flags: u32,
        buffer: *mut u16,
        size: *mut u32,
    ) -> Bool;

    pub fn QueryDosDeviceW(device_name: *const u16, target_path: *mut u16, max: u32) -> u32;

    pub fn GetLogicalDriveStringsW(buffer_length: u32, buffer: *mut u16) -> u32;

    /// Needs a root path with a trailing backslash (`C:\`), which is exactly
    /// what `GetLogicalDriveStringsW` yields.
    pub fn GetDriveTypeW(root_path_name: *const u16) -> u32;

    pub fn CloseHandle(object: Handle) -> Bool;

    pub fn CreateMutexW(
        mutex_attributes: *mut c_void,
        initial_owner: Bool,
        name: *const u16,
    ) -> Handle;

    pub fn GetLastError() -> u32;

    pub fn SetLastError(error_code: u32);

    pub fn GetCurrentProcess() -> Handle;
}

#[link(name = "user32")]
extern "system" {
    pub fn FindWindowW(class_name: *const u16, window_name: *const u16) -> Handle;
    pub fn IsIconic(window: Handle) -> Bool;
    pub fn ShowWindow(window: Handle, command: i32) -> Bool;
    pub fn SetForegroundWindow(window: Handle) -> Bool;

    pub fn MessageBoxW(window: Handle, text: *const u16, caption: *const u16, kind: u32) -> i32;
}

#[link(name = "advapi32")]
extern "system" {
    pub fn OpenProcessToken(process: Handle, desired_access: u32, token: *mut Handle) -> Bool;

    pub fn GetTokenInformation(
        token: Handle,
        information_class: u32,
        information: *mut c_void,
        information_length: u32,
        return_length: *mut u32,
    ) -> Bool;
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
