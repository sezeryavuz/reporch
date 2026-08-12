//! # reporch-git
//!
//! Worktrees, branches and merges — by **shelling out to the real `git` binary** (CLAUDE.md,
//! §13.2).
//!
//! > *"Worktree, rebase and merge semantics are exactly what the user expects only when it is the
//! > real `git`, with their config, hooks, and credential helpers. It also keeps the build
//! > pure-Rust, which matters a great deal for cross-compiled release binaries."*
//!
//! Every command here is constructed as data and run through one choke point, so what reporch ran
//! is always visible in the log and in tests.

#![forbid(unsafe_code)]

pub mod worktree;

pub use worktree::{
    add_worktree, carry_over, clear_exclude_block, current_branch, default_branch, has_changes,
    list_worktrees, remove_worktree, run_hook, CarryOutcome, Worktree,
};

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("could not run git: {0}")]
    Spawn(#[source] std::io::Error),

    #[error("git {args} failed in {cwd} (exit {code}):\n{stderr}")]
    Failed {
        args: String,
        cwd: PathBuf,
        code: i32,
        stderr: String,
    },

    #[error("{path} is not a git repository")]
    NotARepo { path: PathBuf },

    #[error("{0}")]
    Other(String),
}

/// The result of a git invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOutput {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

impl GitOutput {
    pub fn ok(&self) -> bool {
        self.code == 0
    }

    /// stdout with trailing whitespace removed — what almost every caller wants.
    pub fn trimmed(&self) -> &str {
        self.stdout.trim()
    }

    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.stdout.lines().map(str::trim).filter(|l| !l.is_empty())
    }
}

/// Run `git` in `cwd`, returning output whether or not it succeeded.
///
/// The single choke point: every git invocation in reporch goes through here, so all of them are
/// logged identically and none can quietly bypass the convention.
pub fn git_raw(cwd: &Path, args: &[&str]) -> Result<GitOutput, GitError> {
    tracing::debug!(cwd = %cwd.display(), args = ?args, "git");
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        // Never let git open an editor or a credential prompt: reporch may be running nine of
        // these unattended, and a blocked prompt is indistinguishable from a hang.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .output()
        .map_err(GitError::Spawn)?;
    Ok(GitOutput {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        code: out.status.code().unwrap_or(-1),
    })
}

/// Run `git`, treating a non-zero exit as an error.
pub fn git(cwd: &Path, args: &[&str]) -> Result<GitOutput, GitError> {
    let out = git_raw(cwd, args)?;
    if out.ok() {
        Ok(out)
    } else {
        Err(GitError::Failed {
            args: args.join(" "),
            cwd: cwd.to_path_buf(),
            code: out.code,
            stderr: out.stderr.trim().to_string(),
        })
    }
}

/// Whether `path` is inside a git working tree.
pub fn is_repo(path: &Path) -> bool {
    git_raw(path, &["rev-parse", "--is-inside-work-tree"])
        .map(|o| o.ok() && o.trimmed() == "true")
        .unwrap_or(false)
}

/// The current `HEAD` commit, full SHA.
///
/// This is `base_commit` in `task.json` — merge is impossible without knowing what was branched
/// from (§8.2).
pub fn head_commit(repo: &Path) -> Result<String, GitError> {
    Ok(git(repo, &["rev-parse", "HEAD"])?.trimmed().to_string())
}

/// Short SHA, for display.
pub fn short_commit(repo: &Path) -> Result<String, GitError> {
    Ok(git(repo, &["rev-parse", "--short", "HEAD"])?
        .trimmed()
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway repository with one commit.
    pub(crate) fn fixture() -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]).unwrap();
        git(&repo, &["config", "user.email", "t@t"]).unwrap();
        git(&repo, &["config", "user.name", "t"]).unwrap();
        std::fs::write(repo.join("README.md"), "# fixture\n").unwrap();
        git(&repo, &["add", "-A"]).unwrap();
        git(&repo, &["commit", "-qm", "init"]).unwrap();
        (d, repo)
    }

    #[test]
    fn detects_a_repository() {
        let (d, repo) = fixture();
        assert!(is_repo(&repo));
        assert!(!is_repo(d.path().parent().unwrap()) || !is_repo(&d.path().join("nope")));
    }

    #[test]
    fn reads_head() {
        let (_d, repo) = fixture();
        let full = head_commit(&repo).unwrap();
        assert_eq!(full.len(), 40, "a full SHA: {full}");
        let short = short_commit(&repo).unwrap();
        assert!(full.starts_with(&short));
    }

    #[test]
    fn a_failing_command_names_what_was_run_and_why() {
        let (_d, repo) = fixture();
        let err = git(&repo, &["checkout", "no-such-branch"]).unwrap_err();
        let s = err.to_string();
        assert!(s.contains("checkout no-such-branch"), "{s}");
        assert!(s.contains(&repo.display().to_string()), "{s}");
    }

    #[test]
    fn git_raw_reports_failure_without_erroring() {
        let (_d, repo) = fixture();
        let out = git_raw(&repo, &["checkout", "no-such-branch"]).unwrap();
        assert!(!out.ok());
        assert!(!out.stderr.is_empty());
    }

    #[test]
    fn output_helpers_trim_and_split() {
        let o = GitOutput {
            stdout: "  a  \n\n b \n".into(),
            stderr: String::new(),
            code: 0,
        };
        assert_eq!(o.trimmed(), "a  \n\n b");
        assert_eq!(o.lines().collect::<Vec<_>>(), vec!["a", "b"]);
    }
}
