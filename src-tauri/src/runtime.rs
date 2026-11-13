//! Process-level runtime façade.
//!
//! V2 introduced this module for elevation, single instance and the autostart
//! entry. The implementation now lives in [`crate::platform`] because each of
//! those concerns needs a per-OS answer; this module only re-exports it so the
//! call sites in `lib.rs` / `collector.rs` / `etw.rs` stay unchanged.

pub use crate::platform::{
    acquire_single_instance, autostart_command, autostart_enabled, focus_existing_window,
    is_elevated, set_autostart, MINIMIZED_FLAG,
};
