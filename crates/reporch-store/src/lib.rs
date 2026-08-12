//! # reporch-store
//!
//! The on-disk layout (§8): where things live, how they are written, and how nine concurrent
//! sessions share the same directory without corrupting it.
//!
//! Two rules run through the whole crate:
//!
//! - **Every write is temp-then-rename.** A reader never sees a half-written file.
//! - **Allocate-and-write happens under an advisory lock.** Two sessions filing a brief at the
//!   same instant cannot be handed the same ID (§7.2).

#![forbid(unsafe_code)]

pub mod atomic;
pub mod config;
pub mod mailbox;
pub mod paths;
pub mod time;

pub use atomic::{
    append_line, read_json, read_json_opt, read_string, update_json_locked, write_atomic,
    write_json, write_string, FileLock,
};
pub use config::{Config, LimitsConfig, ScanConfig, SessionConfig, Transport, UiConfig};
pub use mailbox::{
    ack, load_index, parse_brief, read, record_ping, render_brief, send, wrap_untrusted,
};
pub use paths::{home_dir, Paths, TaskPaths};
pub use time::{human_duration, now_rfc3339, now_unix_ms};

use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("could not determine your home directory; set REPORCH_HOME to choose where reporch keeps its data")]
    NoHomeDirectory,

    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{path} is not a usable path: {detail}")]
    BadPath { path: PathBuf, detail: String },

    #[error("could not write {path}: {source}")]
    Serialise {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("{path} is not valid JSON: {source}")]
    Deserialise {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("{path} could not be parsed: {detail}")]
    Config { path: PathBuf, detail: String },

    #[error("could not lock {path}: {source}")]
    Lock {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{0}")]
    Other(String),
}

impl StoreError {
    /// Attach a path to an I/O error. Public because every crate that touches `$REPORCH_HOME`
    /// wants the same "which file was it" framing rather than a bare `io::Error`.
    pub fn io(path: impl AsRef<Path>, source: std::io::Error) -> Self {
        StoreError::Io {
            path: path.as_ref().to_path_buf(),
            source,
        }
    }
}
