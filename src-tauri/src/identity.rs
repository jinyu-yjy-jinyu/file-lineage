//! Short-term file identity: `(volume serial, file index)`.
//!
//! A path is not an identity — the same file can be renamed or moved, and the
//! same path can be a completely different file a second later. Windows gives
//! us `BY_HANDLE_FILE_INFORMATION` (volume serial + 64-bit file index), and
//! Unix gives us `(st_dev, st_ino)` from `stat`. Both are stable across
//! rename/move within a volume, exactly like an inode.
//!
//! Two consequences the product uses:
//!   * a path change with an unchanged identity is a *rename/move*, not a new file;
//!   * two paths with the *same* identity are **hard links**, not copies — deleting
//!     one of them frees nothing and loses nothing, which is a materially
//!     different answer to "删了会不会丢东西".

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    pub volume: u32,
    pub index: u64,
}

impl FileIdentity {
    /// Compact string key, handy for logs and DB-side comparisons.
    pub fn key(&self) -> String {
        format!("{:08x}-{:016x}", self.volume, self.index)
    }
}

/// Query the identity of a file. Returns `None` for directories, missing
/// files, and filesystems that do not expose file indices (FAT32, some network
/// shares) — callers must treat identity as strictly optional.
#[cfg(windows)]
pub fn file_identity(path: &Path) -> Option<FileIdentity> {
    use crate::win32 as w;
    let wide = crate::util::extended_path(path);
    unsafe {
        let handle = w::CreateFileW(
            wide.as_ptr(),
            0,
            w::FILE_SHARE_READ | w::FILE_SHARE_WRITE | w::FILE_SHARE_DELETE,
            std::ptr::null_mut(),
            w::OPEN_EXISTING,
            w::FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        );
        if handle.is_null() || handle == w::INVALID_HANDLE_VALUE {
            return None;
        }
        let mut info: w::ByHandleFileInformation = std::mem::zeroed();
        let ok = w::GetFileInformationByHandle(handle, &mut info);
        w::CloseHandle(handle);
        if ok == 0 {
            return None;
        }
        // Directories share the index space; they are never tracked anyway.
        const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
        if info.attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
            return None;
        }
        Some(FileIdentity {
            volume: info.volume_serial,
            index: ((info.index_high as u64) << 32) | info.index_low as u64,
        })
    }
}

/// `(st_dev, st_ino)` is the Unix equivalent of the NTFS FileID: stable across
/// rename/move inside one filesystem, and identical for hard links.
#[cfg(unix)]
pub fn file_identity(path: &Path) -> Option<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    Some(FileIdentity {
        // `st_dev` is wider than the DB's INTEGER on paper but in practice
        // fits in 32 bits; truncation only affects logging/keys.
        volume: meta.dev() as u32,
        index: meta.ino(),
    })
}

#[cfg(not(any(windows, unix)))]
pub fn file_identity(_path: &Path) -> Option<FileIdentity> {
    None
}
