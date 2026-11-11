//! Linux-side one-shot scan helper protocol and runner.
//!
//! The Windows GUI invokes this binary through `wsl.exe` to scan WSL repositories.
//! stdout is reserved for one-line JSON responses, while stderr carries `tracing` logs.
//! Bump [`PROTOCOL_VERSION`] only for breaking wire-format changes.
//! See `crates/wsl-helper/README.md` for operational details.
#![deny(unsafe_code)]

/// Wire-format version shared with the Windows GUI.
/// Bump only when a breaking protocol change is introduced.
pub const PROTOCOL_VERSION: u32 = 1;

/// Helper binary version, derived from this crate's Cargo package version.
pub const HELPER_VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod protocol;
pub mod runner;

pub use protocol::*;
pub use runner::{run_scan, run_scan_root};
