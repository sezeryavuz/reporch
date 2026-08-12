//! Finding every local git repository (§5.3).
//!
//! Uses the `ignore` crate's parallel walker — ripgrep's engine. Two rules make it fast:
//!
//! 1. **A repository is a leaf.** The moment a `.git` is found, stop descending. Nothing inside a
//!    repo is another repo worth finding, and repos are where the file count explodes.
//! 2. **Prune aggressively.** `node_modules`, `target`, `.venv` and friends never contain a repo
//!    the user means, and they contain most of the files on the machine.
//!
//! Spike #9 measured 149 repos in 0.27s cold with a single-threaded Python prototype, so the
//! §5.3 cache is an optimisation rather than a dependency — a cache miss must never be an error.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ignore::{WalkBuilder, WalkState};
use serde::{Deserialize, Serialize};

/// One discovered repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoundRepo {
    /// The directory name — this is a repo's identity throughout reporch (§3).
    pub name: String,
    pub path: PathBuf,
    /// From `.git/config`, when there is an `origin`.
    #[serde(default)]
    pub remote_url: Option<String>,
    /// The owner or org parsed out of the remote — `example-org` for
    /// `git@github.com:example-org/modulex.git`. A far stronger grouping signal than the
    /// directory name (§5.3).
    #[serde(default)]
    pub remote_org: Option<String>,
    /// Seconds since the HEAD commit, for the recency signal. `None` when it cannot be read
    /// cheaply.
    #[serde(default)]
    pub head_age_secs: Option<u64>,
}

impl FoundRepo {
    /// The parent directory. Siblings of an already-selected repo score in §5.3's ranking.
    pub fn parent(&self) -> Option<&Path> {
        self.path.parent()
    }
}

/// How the scan is bounded.
#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub roots: Vec<PathBuf>,
    pub max_depth: usize,
    pub skip: BTreeSet<String>,
    /// Worker threads. `0` means "as many as the machine has".
    pub threads: usize,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            max_depth: 8,
            skip: BTreeSet::new(),
            threads: 0,
        }
    }
}

/// Walk the roots and return every git repository found, sorted by path.
pub fn scan(opts: &ScanOptions) -> Vec<FoundRepo> {
    let found: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(Vec::new()));
    let Some((first, rest)) = opts.roots.split_first() else {
        return Vec::new();
    };

    // A root can itself be a repository, and the walker's entry filter is not guaranteed to be
    // applied to the roots, so they are checked directly.
    for r in &opts.roots {
        if is_repo_dir(r) {
            if let Ok(mut g) = found.lock() {
                g.push(r.clone());
            }
        }
    }

    let mut builder = WalkBuilder::new(first);
    for r in rest {
        builder.add(r);
    }
    builder
        .max_depth(Some(opts.max_depth))
        // reporch is looking for repositories, not source files: a repo ignored by a parent
        // .gitignore is still a repo the user may want, so none of the ignore machinery applies.
        .standard_filters(false)
        .hidden(false)
        .follow_links(false)
        .threads(opts.threads);

    let skip = opts.skip.clone();
    let collect = Arc::clone(&found);
    // The prune happens here rather than in the visitor, because returning `WalkState::Skip` on
    // seeing `.git` only stops descent into `.git` itself — the walker would still descend into
    // the repository's other subdirectories and find vendored repos inside it. Pruning the
    // repository *directory* is what makes "a repo is a leaf" true, and it is what keeps the
    // scan fast.
    builder.filter_entry(move |e| {
        // Depth 0 is a root the user configured explicitly. Never filter it by name: a root
        // may legitimately be hidden (`~/.local/src`) or share a name with the skip list, and
        // silently scanning nothing would look like "you have no repositories".
        if e.depth() == 0 {
            return true;
        }
        let name = e.file_name().to_string_lossy();
        if skip.contains(name.as_ref()) {
            return false;
        }
        if name.starts_with('.') {
            return false;
        }
        if e.file_type().is_some_and(|t| t.is_dir()) && is_repo_dir(e.path()) {
            if let Ok(mut g) = collect.lock() {
                g.push(e.path().to_path_buf());
            }
            // Prune: nothing inside a repository is another repository the user means.
            return false;
        }
        true
    });

    builder.build_parallel().run(|| {
        // Everything is collected in the entry filter; the visitor only has to keep walking.
        // An unreadable directory is normal on a real machine (permissions, dead symlinks,
        // network mounts) — skip it, never fail the scan.
        Box::new(move |_result| WalkState::Continue)
    });

    // Lock and clone rather than `Arc::try_unwrap`: the entry filter installed on the builder
    // holds a second reference, so try_unwrap would fail and — with `unwrap_or_default` — turn a
    // successful scan into a silent empty result.
    let mut paths = found.lock().map(|g| g.clone()).unwrap_or_default();
    paths.sort();
    paths.dedup();

    paths.iter().map(|p| describe(p)).collect()
}

/// Whether a directory is a git repository — `.git` as a directory (normal) or as a file
/// (a worktree).
pub fn is_repo_dir(dir: &Path) -> bool {
    std::fs::metadata(dir.join(".git")).is_ok()
}

/// Read the cheap facts about a repository directly, without shelling out.
///
/// `git` is the right tool for anything that *changes* a repository (CLAUDE.md), but running it
/// once per repo across ~150 repos costs seconds of process spawns to read two strings. Parsing
/// `.git/config` is a cheap read of a small INI file, which is exactly the exception the
/// convention allows. See D009 in `docs/decisions.md`.
pub fn describe(path: &Path) -> FoundRepo {
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());
    let remote_url = read_origin_url(path);
    let remote_org = remote_url.as_deref().and_then(parse_org);
    FoundRepo {
        name,
        path: path.to_path_buf(),
        remote_url,
        remote_org,
        head_age_secs: head_age_secs(path),
    }
}

/// The `origin` remote URL from `.git/config`, if any.
fn read_origin_url(repo: &Path) -> Option<String> {
    let cfg = git_dir(repo)?.join("config");
    let text = std::fs::read_to_string(cfg).ok()?;
    let mut in_origin = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            // `[remote "origin"]`, with any spacing.
            in_origin = t.replace(char::is_whitespace, "") == "[remote\"origin\"]";
            continue;
        }
        if in_origin {
            if let Some(v) = t.strip_prefix("url") {
                if let Some(v) = v.trim_start().strip_prefix('=') {
                    let v = v.trim();
                    if !v.is_empty() {
                        return Some(v.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Resolve the real git directory: a normal repo has `.git/` as a directory, a **worktree** has
/// `.git` as a file containing `gitdir: <path>`.
fn git_dir(repo: &Path) -> Option<PathBuf> {
    let dot = repo.join(".git");
    let meta = std::fs::metadata(&dot).ok()?;
    if meta.is_dir() {
        return Some(dot);
    }
    let contents = std::fs::read_to_string(&dot).ok()?;
    let rest = contents.trim().strip_prefix("gitdir:")?.trim();
    let p = PathBuf::from(rest);
    Some(if p.is_absolute() { p } else { repo.join(p) })
}

/// The owner/org from a git remote URL.
///
/// Handles the three forms in the wild:
/// `git@github.com:org/repo.git`, `https://github.com/org/repo.git`, `ssh://git@host/org/repo`.
pub fn parse_org(url: &str) -> Option<String> {
    let url = url.trim();
    // scp-like: git@host:org/repo.git
    let after_host = if let Some(idx) = url.find("://") {
        let rest = &url[idx + 3..];
        let slash = rest.find('/')?;
        &rest[slash + 1..]
    } else if let Some(colon) = url.rfind(':') {
        // Avoid treating a port in a URL-ish string as the separator.
        &url[colon + 1..]
    } else {
        return None;
    };
    let mut parts = after_host.trim_start_matches('/').split('/');
    let org = parts.next()?.trim();
    // A bare `host:repo.git` has no org.
    parts.next()?;
    if org.is_empty() {
        None
    } else {
        Some(org.to_string())
    }
}

/// Age of the HEAD commit, read from the reflog or the commit object's mtime.
///
/// Deliberately approximate: this drives a 0–20 point ranking signal ("demote abandoned
/// checkouts"), not anything a decision depends on. `None` when it cannot be read cheaply.
fn head_age_secs(repo: &Path) -> Option<u64> {
    let git = git_dir(repo)?;
    // The reflog's mtime tracks the last ref update and is a single stat.
    for candidate in ["logs/HEAD", "HEAD"] {
        if let Ok(meta) = std::fs::metadata(git.join(candidate)) {
            if let Ok(modified) = meta.modified() {
                if let Ok(d) = std::time::SystemTime::now().duration_since(modified) {
                    return Some(d.as_secs());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mkrepo(root: &Path, name: &str, origin: Option<&str>) -> PathBuf {
        let p = root.join(name);
        std::fs::create_dir_all(p.join(".git")).unwrap();
        if let Some(o) = origin {
            std::fs::write(
                p.join(".git/config"),
                format!("[core]\n\trepositoryformatversion = 0\n[remote \"origin\"]\n\turl = {o}\n\tfetch = +refs/heads/*\n"),
            )
            .unwrap();
        }
        std::fs::write(p.join("README.md"), "x").unwrap();
        p
    }

    fn opts(root: &Path) -> ScanOptions {
        ScanOptions {
            roots: vec![root.to_path_buf()],
            max_depth: 8,
            skip: ["node_modules", "target", ".venv"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            threads: 2,
        }
    }

    #[test]
    fn finds_repositories_and_names_them_by_directory() {
        let d = tempfile::tempdir().unwrap();
        mkrepo(d.path(), "modulex", None);
        mkrepo(d.path(), "modulex-ui", None);
        std::fs::create_dir_all(d.path().join("not-a-repo")).unwrap();

        let found = scan(&opts(d.path()));
        let names: Vec<&str> = found.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["modulex", "modulex-ui"]);
    }

    /// The rule that makes the scan fast: a repository is a leaf.
    #[test]
    fn does_not_descend_into_a_repository() {
        let d = tempfile::tempdir().unwrap();
        let outer = mkrepo(d.path(), "outer", None);
        // A vendored repo inside another repo must not be reported separately.
        mkrepo(&outer, "vendored", None);

        let found = scan(&opts(d.path()));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "outer");
    }

    #[test]
    fn prunes_the_skip_list() {
        let d = tempfile::tempdir().unwrap();
        mkrepo(&d.path().join("node_modules"), "some-dep", None);
        mkrepo(&d.path().join("target"), "build-artifact", None);
        mkrepo(d.path(), "real", None);

        let found = scan(&opts(d.path()));
        assert_eq!(found.len(), 1, "found: {found:?}");
        assert_eq!(found[0].name, "real");
    }

    #[test]
    fn prunes_hidden_directories_but_not_dot_git_itself() {
        let d = tempfile::tempdir().unwrap();
        mkrepo(&d.path().join(".cache"), "hidden-repo", None);
        mkrepo(d.path(), "visible", None);
        let found = scan(&opts(d.path()));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "visible");
    }

    #[test]
    fn respects_the_depth_bound() {
        let d = tempfile::tempdir().unwrap();
        mkrepo(&d.path().join("a/b/c/d/e/f/g"), "deep", None);
        let mut o = opts(d.path());
        o.max_depth = 3;
        assert!(scan(&o).is_empty());
        o.max_depth = 12;
        assert_eq!(scan(&o).len(), 1);
    }

    #[test]
    fn reads_the_origin_remote_and_org() {
        let d = tempfile::tempdir().unwrap();
        mkrepo(
            d.path(),
            "modulex",
            Some("git@github.com:example-org/modulex.git"),
        );
        let found = scan(&opts(d.path()));
        assert_eq!(
            found[0].remote_url.as_deref(),
            Some("git@github.com:example-org/modulex.git")
        );
        assert_eq!(found[0].remote_org.as_deref(), Some("example-org"));
    }

    #[test]
    fn a_repo_without_a_remote_is_still_found() {
        let d = tempfile::tempdir().unwrap();
        mkrepo(d.path(), "local-only", None);
        let found = scan(&opts(d.path()));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].remote_url, None);
        assert_eq!(found[0].remote_org, None);
    }

    /// A git worktree has `.git` as a *file*, not a directory. reporch creates worktrees itself,
    /// so failing to recognise one would make its own output invisible to its own scanner.
    #[test]
    fn recognises_a_worktree_whose_dot_git_is_a_file() {
        let d = tempfile::tempdir().unwrap();
        let real = mkrepo(
            d.path(),
            "main-checkout",
            Some("git@github.com:org/repo.git"),
        );
        let wt = d.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", real.join(".git").display()),
        )
        .unwrap();

        let found = scan(&opts(d.path()));
        let wt_entry = found
            .iter()
            .find(|r| r.name == "wt")
            .expect("worktree found");
        assert_eq!(wt_entry.remote_org.as_deref(), Some("org"));
    }

    #[test]
    fn parses_every_remote_url_form() {
        for (url, org) in [
            (
                "git@github.com:example-org/modulex.git",
                Some("example-org"),
            ),
            (
                "https://github.com/example-org/modulex.git",
                Some("example-org"),
            ),
            (
                "https://github.com/example-org/modulex",
                Some("example-org"),
            ),
            (
                "ssh://git@github.com/example-org/modulex.git",
                Some("example-org"),
            ),
            ("git@gitlab.com:group/sub/proj.git", Some("group")),
            ("https://user:token@github.com/org/repo.git", Some("org")),
        ] {
            assert_eq!(parse_org(url).as_deref(), org, "for {url}");
        }
    }

    #[test]
    fn returns_none_for_unparseable_remotes() {
        for url in ["", "not a url", "/local/path", "github.com"] {
            assert_eq!(parse_org(url), None, "for {url:?}");
        }
    }

    #[test]
    fn an_empty_root_list_is_not_an_error() {
        assert!(scan(&ScanOptions::default()).is_empty());
    }

    #[test]
    fn a_nonexistent_root_is_not_an_error() {
        let o = ScanOptions {
            roots: vec![PathBuf::from("/definitely/not/here/at/all")],
            ..opts(Path::new("/tmp"))
        };
        assert!(scan(&o).is_empty());
    }

    #[test]
    fn results_are_sorted_and_deduplicated() {
        let d = tempfile::tempdir().unwrap();
        mkrepo(d.path(), "zeta", None);
        mkrepo(d.path(), "alpha", None);
        // The same root twice must not double-report.
        let o = ScanOptions {
            roots: vec![d.path().to_path_buf(), d.path().to_path_buf()],
            ..opts(d.path())
        };
        let found = scan(&o);
        let names: Vec<&str> = found.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "zeta"]);
    }
}
