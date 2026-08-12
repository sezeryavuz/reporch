//! The discovery cache (§5.3).
//!
//! Spike #9 measured the real scan at 0.27s, so this is an **optimisation, not a dependency**:
//! every failure path here returns "no cache" rather than an error, and a stale cache is simply
//! ignored. A cache that can break the product is worse than no cache.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::discover::FoundRepo;

pub const CACHE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepoCache {
    pub schema_version: u32,
    /// The roots this cache was built from. Different roots mean a different answer.
    pub roots: Vec<PathBuf>,
    pub max_depth: usize,
    /// Root mtimes at capture time. Any change invalidates (§5.3).
    pub root_mtimes: Vec<u64>,
    pub captured_at_unix: u64,
    pub repos: Vec<FoundRepo>,
}

impl RepoCache {
    pub fn build(roots: &[PathBuf], max_depth: usize, repos: Vec<FoundRepo>) -> Self {
        Self {
            schema_version: CACHE_SCHEMA_VERSION,
            roots: roots.to_vec(),
            max_depth,
            root_mtimes: roots.iter().map(|r| mtime(r)).collect(),
            captured_at_unix: now_unix(),
            repos,
        }
    }

    /// Whether this cache still describes the same question, and the roots have not changed.
    pub fn is_valid_for(&self, roots: &[PathBuf], max_depth: usize) -> bool {
        self.schema_version == CACHE_SCHEMA_VERSION
            && self.roots == roots
            && self.max_depth == max_depth
            && self.root_mtimes.len() == roots.len()
            && roots
                .iter()
                .zip(&self.root_mtimes)
                .all(|(r, m)| mtime(r) == *m)
    }

    /// Age in seconds. Shown to the user so a surprising list is explicable.
    pub fn age_secs(&self) -> u64 {
        now_unix().saturating_sub(self.captured_at_unix)
    }
}

fn mtime(p: &Path) -> u64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repos() -> Vec<FoundRepo> {
        vec![FoundRepo {
            name: "a".into(),
            path: PathBuf::from("/r/a"),
            remote_url: None,
            remote_org: None,
            head_age_secs: None,
        }]
    }

    #[test]
    fn a_fresh_cache_is_valid_for_the_same_question() {
        let d = tempfile::tempdir().unwrap();
        let roots = vec![d.path().to_path_buf()];
        let c = RepoCache::build(&roots, 8, repos());
        assert!(c.is_valid_for(&roots, 8));
    }

    #[test]
    fn different_roots_or_depth_invalidate() {
        let d = tempfile::tempdir().unwrap();
        let roots = vec![d.path().to_path_buf()];
        let c = RepoCache::build(&roots, 8, repos());
        assert!(!c.is_valid_for(&roots, 12), "depth is part of the question");
        assert!(!c.is_valid_for(&[PathBuf::from("/other")], 8));
        assert!(!c.is_valid_for(&[], 8));
    }

    #[test]
    fn a_changed_root_mtime_invalidates() {
        let d = tempfile::tempdir().unwrap();
        let roots = vec![d.path().to_path_buf()];
        let mut c = RepoCache::build(&roots, 8, repos());
        assert!(c.is_valid_for(&roots, 8));
        // Simulate the root having been touched since capture.
        c.root_mtimes = vec![1];
        assert!(!c.is_valid_for(&roots, 8));
    }

    #[test]
    fn an_old_schema_invalidates() {
        let d = tempfile::tempdir().unwrap();
        let roots = vec![d.path().to_path_buf()];
        let mut c = RepoCache::build(&roots, 8, repos());
        c.schema_version = 0;
        assert!(!c.is_valid_for(&roots, 8));
    }

    #[test]
    fn a_missing_root_does_not_panic() {
        let roots = vec![PathBuf::from("/definitely/not/here")];
        let c = RepoCache::build(&roots, 8, repos());
        // Both sides read 0 for a missing path, so it is self-consistent rather than a crash.
        assert!(c.is_valid_for(&roots, 8));
    }

    #[test]
    fn round_trips_through_json() {
        let d = tempfile::tempdir().unwrap();
        let c = RepoCache::build(&[d.path().to_path_buf()], 8, repos());
        let s = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<RepoCache>(&s).unwrap(), c);
    }
}
