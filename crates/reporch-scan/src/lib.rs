//! # reporch-scan
//!
//! Repository discovery and ranking (§5.3).
//!
//! Discovery is a pruned parallel walk; ranking is a weighted score that re-runs on every
//! selection change, so choosing one repo immediately pulls its family up the list.
//!
//! ## Discovery
//!
//! [`scan()`] treats a repository as a leaf, skips known bulk directories and reads cheap Git facts
//! without spawning one process per candidate. [`RepoCache`] keys the result by roots, depth,
//! schema and root mtimes; the cache is an optimization, never the source of truth.
//!
//! ## Ranking
//!
//! [`rank()`] combines project-name similarity, shared remote organization, filesystem proximity
//! and recency. [`Selection`] feeds already chosen repositories back into that score, which is why
//! selecting one member of a repository family promotes its siblings immediately. Ties break by
//! name and every input is explicit, so the same scan and selection produce the same order.

#![forbid(unsafe_code)]

pub mod cache;
pub mod discover;
pub mod rank;

pub use cache::RepoCache;
pub use discover::{scan, FoundRepo, ScanOptions};
pub use rank::{filter, rank, Ranked, Selection};

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("{path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
}
