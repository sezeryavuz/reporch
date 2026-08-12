//! Worktrees (§9).
//!
//! Worktrees live centrally under `~/.reporch/worktrees/<project>/<TASK_ID>/<repo>`, not inside
//! the user's repositories: cleanup is one `rm -rf` plus `git worktree prune`, the user's repo
//! directories stay clean, and repos remain **siblings of each other**, so relative cross-repo
//! paths like `../modulex-integrations` keep working (§9.1).
//!
//! §9.2 is the part that breaks first in real use: `git worktree add` gives a clean checkout of
//! **tracked files only**. No `.env`, no `node_modules`, no `.venv`. Nine worktrees where nothing
//! builds, and nine sessions that discover it one at a time and start improvising fixes with full
//! shell access.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{git, git_raw, GitError};

/// A worktree reporch created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: String,
    /// The commit the branch was created from. Merge is impossible without it (§8.2).
    pub base_commit: String,
}

/// The repository's default branch — what a task branch is created from and merged back into.
///
/// Tries the remote's HEAD, then a local `main`/`master`, then whatever is checked out. Falling
/// all the way through to the current branch is deliberate: a repo with an unusual default is
/// still usable, and guessing `main` when it does not exist produces a much worse error later.
pub fn default_branch(repo: &Path) -> Result<String, GitError> {
    // `origin/HEAD` is the authoritative answer when the remote has been fetched.
    if let Ok(o) = git_raw(
        repo,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    ) {
        if o.ok() {
            if let Some(b) = o.trimmed().strip_prefix("origin/") {
                if !b.is_empty() {
                    return Ok(b.to_string());
                }
            }
        }
    }
    for candidate in ["main", "master"] {
        if let Ok(o) = git_raw(
            repo,
            &[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{candidate}"),
            ],
        ) {
            if o.ok() {
                return Ok(candidate.to_string());
            }
        }
    }
    current_branch(repo)
}

/// The branch currently checked out.
pub fn current_branch(repo: &Path) -> Result<String, GitError> {
    Ok(git(repo, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trimmed()
        .to_string())
}

/// Create a worktree on a new branch (§9.1).
///
/// ```text
/// git worktree add -b reporch/TASK-0001-<slug> <path> <base>
/// ```
///
/// Idempotent: if the worktree already exists at that path on that branch, it is adopted rather
/// than failing. Teardown is resumable and the user may close the laptop mid-task (§11.3), so
/// re-running must be safe.
pub fn add_worktree(
    repo: &Path,
    path: &Path,
    branch: &str,
    base: &str,
) -> Result<Worktree, GitError> {
    if path.join(".git").exists() {
        // Already there. Confirm it is the branch we expect before adopting it.
        let existing = current_branch(path)?;
        if existing != branch {
            return Err(GitError::Other(format!(
                "{} already exists on branch `{existing}`, but this task needs `{branch}`. \
                 Remove it, or finish the task that owns it.",
                path.display()
            )));
        }
        let base_commit = crate::head_commit(path)?;
        return Ok(Worktree {
            path: path.to_path_buf(),
            branch: branch.to_string(),
            base_commit,
        });
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| GitError::Other(format!("could not create {}: {e}", parent.display())))?;
    }

    // A crash can remove the linked directory without removing Git's administrative entry.
    // Pruning here is safe for live worktrees and lets the branch-reuse path below recover the
    // stale case without asking the user to repair Git metadata by hand (§11.3, D013).
    let _ = git_raw(repo, &["worktree", "prune"]);

    let base_commit = git(repo, &["rev-parse", base])?.trimmed().to_string();
    let path_s = path.to_string_lossy().to_string();

    let out = git_raw(repo, &["worktree", "add", "-b", branch, &path_s, base])?;
    if !out.ok() {
        // A leftover branch from an aborted run is the common case, and it has a clean recovery:
        // reuse the branch rather than creating it.
        if out.stderr.contains("already exists") {
            let retry = git_raw(repo, &["worktree", "add", &path_s, branch])?;
            if !retry.ok() {
                return Err(GitError::Failed {
                    args: format!("worktree add {path_s} {branch}"),
                    cwd: repo.to_path_buf(),
                    code: retry.code,
                    stderr: retry.stderr.trim().to_string(),
                });
            }
        } else {
            return Err(GitError::Failed {
                args: format!("worktree add -b {branch} {path_s} {base}"),
                cwd: repo.to_path_buf(),
                code: out.code,
                stderr: out.stderr.trim().to_string(),
            });
        }
    }

    Ok(Worktree {
        path: path.to_path_buf(),
        branch: branch.to_string(),
        base_commit,
    })
}

/// Remove a worktree and prune the administrative entry (§11.3).
///
/// Idempotent and resumable: a worktree that is already gone is a success, because teardown may
/// be re-run after a crash.
pub fn remove_worktree(repo: &Path, path: &Path) -> Result<(), GitError> {
    let path_s = path.to_string_lossy().to_string();
    if path.exists() {
        let out = git_raw(repo, &["worktree", "remove", "--force", &path_s])?;
        if !out.ok() && path.exists() {
            // git refuses in some states (e.g. the directory was moved). The central layout is
            // what makes the fallback safe: this path is under $REPORCH_HOME/worktrees, created
            // by reporch, and never inside the user's repository.
            std::fs::remove_dir_all(path).map_err(|e| {
                GitError::Other(format!("could not remove {}: {e}", path.display()))
            })?;
        }
    }
    let _ = git_raw(repo, &["worktree", "prune"]);
    Ok(())
}

/// Every worktree registered for this repository.
pub fn list_worktrees(repo: &Path) -> Result<Vec<PathBuf>, GitError> {
    let out = git(repo, &["worktree", "list", "--porcelain"])?;
    Ok(out
        .stdout
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .map(PathBuf::from)
        .collect())
}

/// Whether the worktree has any change against its base — tracked or untracked.
///
/// This is what fills `changed` in `task.json` and decides whether a repo appears in the merge
/// (§8.2, §11.1).
pub fn has_changes(worktree: &Path, base_commit: &str) -> Result<bool, GitError> {
    let dirty = git(worktree, &["status", "--porcelain"])?;
    if !dirty.trimmed().is_empty() {
        return Ok(true);
    }
    let ahead = git(
        worktree,
        &["rev-list", "--count", &format!("{base_commit}..HEAD")],
    )?;
    Ok(ahead.trimmed() != "0")
}

/// What happened to one carried-over path (§9.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CarryOutcome {
    Copied {
        relative: String,
    },
    Symlinked {
        relative: String,
    },
    Cloned {
        relative: String,
    },
    /// Not present in the source repo — normal, not an error.
    Absent {
        relative: String,
    },
    Failed {
        relative: String,
        reason: String,
    },
}

impl CarryOutcome {
    pub fn is_failure(&self) -> bool {
        matches!(self, CarryOutcome::Failed { .. })
    }
}

/// Carry untracked-but-needed files into a fresh worktree (§9.2).
///
/// `git worktree add` checks out tracked files only, so `.env`, `node_modules` and `.venv` are
/// absent and the project does not build. A worktree where the project does not build is a failed
/// worktree.
///
/// Carried paths are added to `.git/info/exclude` **in the worktree**, so they never show up as
/// untracked noise in the session's `git status` and never end up committed (§9.2, and §7.3's
/// rule that reporch leaves no diff).
pub fn carry_over(
    source_repo: &Path,
    worktree: &Path,
    entries: &[(String, reporch_core::CarryMode)],
) -> Vec<CarryOutcome> {
    let mut outcomes = Vec::new();
    let mut excluded = Vec::new();

    for (pattern, mode) in entries {
        let src = source_repo.join(pattern);
        let dst = worktree.join(pattern);
        if !src.exists() {
            outcomes.push(CarryOutcome::Absent {
                relative: pattern.clone(),
            });
            continue;
        }
        if dst.exists() {
            // Already present — tracked after all, or a re-run. Leave it alone.
            outcomes.push(CarryOutcome::Absent {
                relative: pattern.clone(),
            });
            continue;
        }
        if let Some(parent) = dst.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                outcomes.push(CarryOutcome::Failed {
                    relative: pattern.clone(),
                    reason: e.to_string(),
                });
                continue;
            }
        }

        let outcome = match mode {
            reporch_core::CarryMode::Symlink => symlink(&src, &dst)
                .map(|()| CarryOutcome::Symlinked {
                    relative: pattern.clone(),
                })
                .unwrap_or_else(|e| CarryOutcome::Failed {
                    relative: pattern.clone(),
                    reason: e,
                }),
            reporch_core::CarryMode::Copy => copy_path(&src, &dst)
                .map(|()| CarryOutcome::Copied {
                    relative: pattern.clone(),
                })
                .unwrap_or_else(|e| CarryOutcome::Failed {
                    relative: pattern.clone(),
                    reason: e,
                }),
            reporch_core::CarryMode::CopyOnWrite => {
                // APFS `clonefile` / Btrfs reflink via `cp -c` / `cp --reflink`. A CoW clone is
                // the right default for a large directory like `node_modules`: fast like a
                // symlink, but isolated, so a worktree installing a different dependency version
                // cannot mutate the user's main checkout.
                match clone_cow(&src, &dst) {
                    Ok(()) => CarryOutcome::Cloned {
                        relative: pattern.clone(),
                    },
                    Err(_) => symlink(&src, &dst)
                        .map(|()| CarryOutcome::Symlinked {
                            relative: pattern.clone(),
                        })
                        .unwrap_or_else(|e| CarryOutcome::Failed {
                            relative: pattern.clone(),
                            reason: e,
                        }),
                }
            }
        };
        if !outcome.is_failure() {
            excluded.push(pattern.clone());
        }
        outcomes.push(outcome);
    }

    if !excluded.is_empty() {
        if let Err(e) = add_to_exclude(worktree, &excluded) {
            tracing::warn!(error = %e, "could not update .git/info/exclude");
        }
    }
    outcomes
}

/// Marker lines delimiting reporch's block in `info/exclude`, so teardown removes exactly what
/// reporch added and nothing the user wrote.
const EXCLUDE_BEGIN: &str = "# >>> reporch (carried worktree files, §9.2) >>>";
const EXCLUDE_END: &str = "# <<< reporch <<<";

/// The `info/exclude` git actually reads.
///
/// > **Deviation from §9.2.** The spec says carried files are added to "`.git/info/exclude` in
/// > the worktree". Verified on git 2.39: git does **not** read `info/exclude` from a linked
/// > worktree's own git directory (`.git/worktrees/<name>/info/exclude`) — the file is looked up
/// > in the **common** directory, so the only location that works is the shared
/// > `<repo>/.git/info/exclude`.
/// >
/// > **What changes:** the exclusion is repository-wide rather than worktree-local, which means
/// > it also hides those paths in the user's own checkout while the task runs. That is acceptable
/// > and it is what CLAUDE.md sanctions — `info/exclude` is untracked, so the diff is still
/// > exactly zero — but it is a side effect on the user's repository, so reporch writes a
/// > delimited, self-identifying block and removes it on teardown. Nothing outside that block is
/// > ever touched.
/// >
/// > The alternative, a worktree-scoped `core.excludesFile` via `extensions.worktreeConfig`, does
/// > isolate cleanly, but it mutates `<repo>/.git/config` to enable a repository extension, which
/// > is a larger and less reversible intrusion than a marked block in a file git treats as
/// > scratch space.
///
/// The exclusion matters for **safety**, not tidiness: a session that runs `git add -A` with an
/// unexcluded carried `.env` would stage a secrets file onto the task branch.
fn exclude_file(worktree: &Path) -> Result<PathBuf, String> {
    let common = git(
        worktree,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .map_err(|e| e.to_string())?
    .trimmed()
    .to_string();
    let info = Path::new(&common).join("info");
    std::fs::create_dir_all(&info).map_err(|e| e.to_string())?;
    Ok(info.join("exclude"))
}

/// Add reporch's block to `info/exclude`, replacing any previous one.
fn add_to_exclude(worktree: &Path, patterns: &[String]) -> Result<(), String> {
    let path = exclude_file(worktree)?;
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut text = strip_reporch_block(&existing);
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(EXCLUDE_BEGIN);
    text.push('\n');
    for p in patterns {
        text.push_str(&format!("/{}\n", p.trim_start_matches('/')));
    }
    text.push_str(EXCLUDE_END);
    text.push('\n');
    std::fs::write(&path, text).map_err(|e| e.to_string())
}

/// Remove reporch's block from `info/exclude`, leaving everything the user wrote intact.
///
/// Called during teardown (§11.3). Idempotent: a file with no block, or no file at all, is a
/// success.
pub fn clear_exclude_block(worktree: &Path) -> Result<(), String> {
    let Ok(path) = exclude_file(worktree) else {
        return Ok(());
    };
    let Ok(existing) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    if !existing.contains(EXCLUDE_BEGIN) {
        return Ok(());
    }
    std::fs::write(&path, strip_reporch_block(&existing)).map_err(|e| e.to_string())
}

fn strip_reporch_block(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut skipping = false;
    for line in text.lines() {
        if line.trim() == EXCLUDE_BEGIN {
            skipping = true;
            continue;
        }
        if skipping {
            if line.trim() == EXCLUDE_END {
                skipping = false;
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn symlink(src: &Path, dst: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(src, dst).map_err(|e| e.to_string())
    }
    #[cfg(not(unix))]
    {
        let _ = (src, dst);
        Err("symlinks are not supported on this platform".to_string())
    }
}

fn copy_path(src: &Path, dst: &Path) -> Result<(), String> {
    let meta = std::fs::metadata(src).map_err(|e| e.to_string())?;
    if meta.is_file() {
        std::fs::copy(src, dst)
            .map(|_| ())
            .map_err(|e| e.to_string())
    } else {
        // `cp -R` rather than a hand-rolled recursive copy: it handles symlinks, permissions and
        // sparse files the way the user expects, and it is present everywhere reporch runs.
        let out = Command::new("cp")
            .arg("-R")
            .arg(src)
            .arg(dst)
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }
}

/// Copy-on-write clone where the filesystem supports it.
fn clone_cow(src: &Path, dst: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let args: Vec<&std::ffi::OsStr> = vec!["-Rc".as_ref(), src.as_ref(), dst.as_ref()];
    #[cfg(not(target_os = "macos"))]
    let args: Vec<&std::ffi::OsStr> = vec![
        "-R".as_ref(),
        "--reflink=always".as_ref(),
        src.as_ref(),
        dst.as_ref(),
    ];

    let out = Command::new("cp")
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        let _ = std::fs::remove_dir_all(dst);
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Run a post-create hook in the worktree, capturing output (§9.2).
///
/// A failing hook **blocks the task** rather than letting a session discover a broken environment
/// and start improvising fixes with full shell access.
pub fn run_hook(worktree: &Path, command: &str) -> Result<String, GitError> {
    let out = Command::new("sh")
        .arg("-lc")
        .arg(command)
        .current_dir(worktree)
        .output()
        .map_err(GitError::Spawn)?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if out.status.success() {
        Ok(stdout)
    } else {
        Err(GitError::Other(format!(
            "post-create hook `{command}` failed in {} (exit {}):\n{}",
            worktree.display(),
            out.status.code().unwrap_or(-1),
            if stderr.trim().is_empty() {
                stdout.trim()
            } else {
                stderr.trim()
            }
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::fixture;
    use reporch_core::CarryMode;

    #[test]
    fn creates_a_worktree_on_a_new_branch() {
        let (d, repo) = fixture();
        let wt = d.path().join("wt/modulex");
        let w = add_worktree(&repo, &wt, "reporch/TASK-0001-x", "main").unwrap();
        assert!(
            wt.join("README.md").is_file(),
            "tracked files are checked out"
        );
        assert_eq!(w.branch, "reporch/TASK-0001-x");
        assert_eq!(current_branch(&wt).unwrap(), "reporch/TASK-0001-x");
        assert_eq!(w.base_commit.len(), 40);
    }

    #[test]
    fn worktrees_for_one_task_are_siblings() {
        let (d, repo) = fixture();
        let a = d.path().join("wt/TASK-0001/modulex");
        let b = d.path().join("wt/TASK-0001/modulex-ui");
        add_worktree(&repo, &a, "reporch/TASK-0001-a", "main").unwrap();
        add_worktree(&repo, &b, "reporch/TASK-0001-b", "main").unwrap();
        assert_eq!(
            a.parent(),
            b.parent(),
            "§9.1: ../modulex-ui must keep working"
        );
    }

    /// Teardown is resumable and the user may close the laptop mid-task, so re-running must be
    /// safe rather than an error.
    #[test]
    fn adding_the_same_worktree_twice_is_idempotent() {
        let (d, repo) = fixture();
        let wt = d.path().join("wt/x");
        let a = add_worktree(&repo, &wt, "reporch/TASK-0001-x", "main").unwrap();
        let b = add_worktree(&repo, &wt, "reporch/TASK-0001-x", "main").unwrap();
        assert_eq!(a.branch, b.branch);
        assert_eq!(a.path, b.path);
    }

    #[test]
    fn adopting_a_worktree_on_the_wrong_branch_is_refused_clearly() {
        let (d, repo) = fixture();
        let wt = d.path().join("wt/x");
        add_worktree(&repo, &wt, "reporch/TASK-0001-x", "main").unwrap();
        let err = add_worktree(&repo, &wt, "reporch/TASK-0002-y", "main").unwrap_err();
        let s = err.to_string();
        assert!(s.contains("TASK-0002"), "{s}");
        assert!(
            s.contains("TASK-0001"),
            "must name what is actually there: {s}"
        );
    }

    /// A branch left behind by an aborted run must not block a retry.
    #[test]
    fn reuses_a_leftover_branch_from_an_aborted_run() {
        let (d, repo) = fixture();
        let wt1 = d.path().join("wt/first");
        add_worktree(&repo, &wt1, "reporch/TASK-0001-x", "main").unwrap();
        remove_worktree(&repo, &wt1).unwrap(); // branch survives
        let wt2 = d.path().join("wt/second");
        let w = add_worktree(&repo, &wt2, "reporch/TASK-0001-x", "main").unwrap();
        assert_eq!(w.branch, "reporch/TASK-0001-x");
        assert!(wt2.join("README.md").is_file());
    }

    /// A process crash or manual removal can leave Git's administrative entry behind even though
    /// the linked worktree directory is gone. Retrying the task must heal that state rather than
    /// require the user to know about `git worktree prune` (§11.3).
    #[test]
    fn a_stale_worktree_registration_does_not_block_recovery() {
        let (d, repo) = fixture();
        let stale = d.path().join("wt/stale");
        add_worktree(&repo, &stale, "reporch/TASK-0001-x", "main").unwrap();

        // Deliberately bypass `remove_worktree`: this reproduces a vanished directory with a
        // still-registered branch, exactly as the baseline E2E fixture was found.
        std::fs::remove_dir_all(&stale).unwrap();

        let replacement = d.path().join("wt/replacement");
        let recovered = add_worktree(&repo, &replacement, "reporch/TASK-0001-x", "main").unwrap();
        assert_eq!(recovered.path, replacement);
        assert!(recovered.path.join("README.md").is_file());
    }

    #[test]
    fn removal_is_idempotent() {
        let (d, repo) = fixture();
        let wt = d.path().join("wt/x");
        add_worktree(&repo, &wt, "reporch/TASK-0001-x", "main").unwrap();
        remove_worktree(&repo, &wt).unwrap();
        assert!(!wt.exists());
        remove_worktree(&repo, &wt).unwrap(); // again: still fine
    }

    #[test]
    fn lists_worktrees() {
        let (d, repo) = fixture();
        let wt = d.path().join("wt/x");
        add_worktree(&repo, &wt, "reporch/TASK-0001-x", "main").unwrap();
        let list = list_worktrees(&repo).unwrap();
        assert!(list.len() >= 2, "the main checkout plus ours: {list:?}");
    }

    #[test]
    fn default_branch_prefers_main() {
        let (_d, repo) = fixture();
        assert_eq!(default_branch(&repo).unwrap(), "main");
    }

    #[test]
    fn detects_changes_and_their_absence() {
        let (d, repo) = fixture();
        let wt = d.path().join("wt/x");
        let w = add_worktree(&repo, &wt, "reporch/TASK-0001-x", "main").unwrap();
        assert!(
            !has_changes(&wt, &w.base_commit).unwrap(),
            "fresh: unchanged"
        );

        std::fs::write(wt.join("new.txt"), "hello").unwrap();
        assert!(
            has_changes(&wt, &w.base_commit).unwrap(),
            "an untracked file counts: it is work the session did"
        );

        git(&wt, &["add", "-A"]).unwrap();
        git(
            &wt,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-qm",
                "x",
            ],
        )
        .unwrap();
        assert!(has_changes(&wt, &w.base_commit).unwrap(), "committed too");
    }

    // ---- §9.2, the untracked-files problem --------------------------------

    #[test]
    fn a_fresh_worktree_is_missing_untracked_files() {
        // The problem §9.2 exists to solve, asserted so the fix is provably needed.
        let (d, repo) = fixture();
        std::fs::write(repo.join(".env"), "SECRET=1\n").unwrap();
        let wt = d.path().join("wt/x");
        add_worktree(&repo, &wt, "reporch/TASK-0001-x", "main").unwrap();
        assert!(
            !wt.join(".env").exists(),
            "git worktree add checks out tracked files only"
        );
    }

    #[test]
    fn copy_carries_a_secrets_file_and_excludes_it() {
        let (d, repo) = fixture();
        std::fs::write(repo.join(".env"), "SECRET=1\n").unwrap();
        let wt = d.path().join("wt/x");
        add_worktree(&repo, &wt, "reporch/TASK-0001-x", "main").unwrap();

        let out = carry_over(&repo, &wt, &[(".env".into(), CarryMode::Copy)]);
        assert_eq!(
            out,
            vec![CarryOutcome::Copied {
                relative: ".env".into()
            }]
        );
        assert_eq!(
            std::fs::read_to_string(wt.join(".env")).unwrap(),
            "SECRET=1\n"
        );

        // And it must not appear as untracked noise, or get committed.
        let status = git(&wt, &["status", "--porcelain"]).unwrap();
        assert!(
            !status.stdout.contains(".env"),
            "carried files go in .git/info/exclude: {}",
            status.stdout
        );
    }

    /// The block must be removable, so reporch leaves the user's repository as it found it.
    #[test]
    fn the_exclude_block_is_delimited_and_removable() {
        let (d, repo) = fixture();
        std::fs::write(repo.join(".env"), "SECRET=1\n").unwrap();
        // Something the user wrote themselves, which must survive untouched.
        let info = repo.join(".git/info");
        std::fs::create_dir_all(&info).unwrap();
        std::fs::write(info.join("exclude"), "# my own rules\n/scratch\n").unwrap();

        let wt = d.path().join("wt/x");
        add_worktree(&repo, &wt, "reporch/TASK-0001-x", "main").unwrap();
        carry_over(&repo, &wt, &[(".env".into(), CarryMode::Copy)]);

        let after = std::fs::read_to_string(info.join("exclude")).unwrap();
        assert!(
            after.contains("/scratch"),
            "the user's rules survive: {after}"
        );
        assert!(after.contains("/.env"), "{after}");

        clear_exclude_block(&wt).unwrap();
        let cleaned = std::fs::read_to_string(info.join("exclude")).unwrap();
        assert!(cleaned.contains("/scratch"), "still there: {cleaned}");
        assert!(
            !cleaned.contains("/.env"),
            "reporch's block is gone: {cleaned}"
        );
        assert!(!cleaned.contains("reporch"), "{cleaned}");

        // Idempotent.
        clear_exclude_block(&wt).unwrap();
    }

    #[test]
    fn symlink_carries_a_large_directory() {
        let (d, repo) = fixture();
        std::fs::create_dir_all(repo.join("node_modules/pkg")).unwrap();
        std::fs::write(repo.join("node_modules/pkg/index.js"), "x").unwrap();
        let wt = d.path().join("wt/x");
        add_worktree(&repo, &wt, "reporch/TASK-0001-x", "main").unwrap();

        let out = carry_over(&repo, &wt, &[("node_modules".into(), CarryMode::Symlink)]);
        assert_eq!(
            out,
            vec![CarryOutcome::Symlinked {
                relative: "node_modules".into()
            }]
        );
        assert!(wt.join("node_modules/pkg/index.js").exists());
    }

    /// The sharp edge §9.2 names: a symlinked `node_modules` is shared with the main checkout, so
    /// a worktree installing a different version mutates it. Copy-on-write isolates instead.
    #[test]
    fn copy_on_write_isolates_where_a_symlink_would_not() {
        let (d, repo) = fixture();
        std::fs::create_dir_all(repo.join("node_modules")).unwrap();
        std::fs::write(repo.join("node_modules/v"), "1").unwrap();
        let wt = d.path().join("wt/x");
        add_worktree(&repo, &wt, "reporch/TASK-0001-x", "main").unwrap();

        let out = carry_over(
            &repo,
            &wt,
            &[("node_modules".into(), CarryMode::CopyOnWrite)],
        );
        assert!(!out[0].is_failure(), "{out:?}");
        assert_eq!(
            std::fs::read_to_string(wt.join("node_modules/v")).unwrap(),
            "1"
        );

        // Mutating the worktree's copy must not touch the user's checkout.
        if matches!(out[0], CarryOutcome::Cloned { .. }) {
            std::fs::write(wt.join("node_modules/v"), "2").unwrap();
            assert_eq!(
                std::fs::read_to_string(repo.join("node_modules/v")).unwrap(),
                "1",
                "a CoW clone must be isolated from the source"
            );
        }
    }

    #[test]
    fn a_missing_carry_over_source_is_not_a_failure() {
        let (d, repo) = fixture();
        let wt = d.path().join("wt/x");
        add_worktree(&repo, &wt, "reporch/TASK-0001-x", "main").unwrap();
        let out = carry_over(&repo, &wt, &[(".env.local".into(), CarryMode::Copy)]);
        assert_eq!(
            out,
            vec![CarryOutcome::Absent {
                relative: ".env.local".into()
            }],
            "most repos do not have most of these; that is normal"
        );
    }

    // ---- post-create hooks -------------------------------------------------

    #[test]
    fn a_hook_runs_in_the_worktree() {
        let (d, repo) = fixture();
        let wt = d.path().join("wt/x");
        add_worktree(&repo, &wt, "reporch/TASK-0001-x", "main").unwrap();
        run_hook(&wt, "touch built.marker").unwrap();
        assert!(
            wt.join("built.marker").exists(),
            "hooks run in the worktree"
        );
    }

    /// A worktree where the project does not build is a failed worktree (§9.2), so the failure
    /// must surface with enough detail to fix it.
    #[test]
    fn a_failing_hook_is_an_error_that_shows_the_output() {
        let (d, repo) = fixture();
        let wt = d.path().join("wt/x");
        add_worktree(&repo, &wt, "reporch/TASK-0001-x", "main").unwrap();
        let err = run_hook(&wt, "echo 'lockfile is out of date' >&2; exit 3").unwrap_err();
        let s = err.to_string();
        assert!(s.contains("lockfile is out of date"), "{s}");
        assert!(s.contains("exit 3"), "{s}");
    }
}
