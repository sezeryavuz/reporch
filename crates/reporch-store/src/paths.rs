//! The on-disk layout (§8).
//!
//! Root is `$REPORCH_HOME`, defaulting to `~/.reporch` — not XDG-buried, because users of a tool
//! like this will want to open these files.

use std::path::{Path, PathBuf};

use reporch_core::ids::{task_dir_name, TaskId};

use crate::StoreError;

/// Every path reporch reads or writes, derived from one root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    root: PathBuf,
}

impl Paths {
    /// Resolve the root: `$REPORCH_HOME` if set, else `~/.reporch`.
    ///
    /// The env var is the documented override and is what the test suite and every spawned
    /// session use, so it must win unconditionally.
    pub fn resolve() -> Result<Self, StoreError> {
        match std::env::var_os("REPORCH_HOME") {
            Some(v) if !v.is_empty() => Ok(Self::at(PathBuf::from(v))),
            _ => {
                let home = home_dir().ok_or(StoreError::NoHomeDirectory)?;
                Ok(Self::at(home.join(".reporch")))
            }
        }
    }

    /// Root at an explicit location. Used by tests and by `--home`.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let root = if root.is_absolute() {
            root
        } else {
            // Git resolves a relative `worktree add <path>` from the repository passed as its
            // cwd, not from reporch's cwd. Anchor once at the storage boundary so every later
            // process sees the same location (D016).
            std::path::absolute(&root).unwrap_or(root)
        };
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn config_file(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.root.join("cache")
    }

    /// §5.3's repo-discovery cache. An optimisation, not load-bearing — spike #9 measured the
    /// real scan at 0.27s.
    pub fn repo_cache_file(&self) -> PathBuf {
        self.cache_dir().join("repos.json")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    /// **The only place reporch logs.** Never stdout, never stderr: one corrupts the TUI, the
    /// other poisons the `--json` output that agents parse (CLAUDE.md).
    pub fn log_file(&self) -> PathBuf {
        self.logs_dir().join("reporch.log")
    }

    pub fn projects_dir(&self) -> PathBuf {
        self.root.join("projects")
    }

    pub fn project_dir(&self, project: &str) -> PathBuf {
        self.projects_dir().join(project)
    }

    pub fn project_base_json(&self, project: &str) -> PathBuf {
        self.project_dir(project).join("project_base.json")
    }

    pub fn project_base_md(&self, project: &str) -> PathBuf {
        self.project_dir(project).join("project_base.md")
    }

    pub fn repos_dir(&self, project: &str) -> PathBuf {
        self.project_dir(project).join("repos")
    }

    pub fn repo_profile_json(&self, project: &str, repo: &str) -> PathBuf {
        self.repos_dir(project).join(format!("{repo}.json"))
    }

    pub fn repo_profile_md(&self, project: &str, repo: &str) -> PathBuf {
        self.repos_dir(project).join(format!("{repo}.md"))
    }

    pub fn tasks_dir(&self, project: &str) -> PathBuf {
        self.project_dir(project).join("tasks")
    }

    /// `<slug>-<TASK_ID>` (§8).
    pub fn task_dir(&self, project: &str, slug: &str, id: TaskId) -> PathBuf {
        self.tasks_dir(project).join(task_dir_name(slug, id))
    }

    /// Worktrees live centrally, not inside the user's repos (§9.1): cleanup is one `rm -rf` plus
    /// `git worktree prune`, and repos stay siblings of each other so relative cross-repo paths
    /// keep working.
    pub fn worktrees_dir(&self) -> PathBuf {
        self.root.join("worktrees")
    }

    pub fn task_worktrees_dir(&self, project: &str, id: TaskId) -> PathBuf {
        self.worktrees_dir().join(project).join(id.to_string())
    }

    pub fn worktree_path(&self, project: &str, id: TaskId, repo: &str) -> PathBuf {
        self.task_worktrees_dir(project, id).join(repo)
    }
}

/// Paths inside one task directory (§8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskPaths {
    dir: PathBuf,
}

impl TaskPaths {
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn task_json(&self) -> PathBuf {
        self.dir.join("task.json")
    }

    /// The user's raw brief, verbatim.
    pub fn base_md(&self) -> PathBuf {
        self.dir.join("base.md")
    }

    /// The exact protocol text given to this task's sessions.
    ///
    /// Written once per task so the rules are readable, greppable and versioned with the task —
    /// and outside the user's repo (§7.3 Deviation).
    pub fn protocol_md(&self) -> PathBuf {
        self.dir.join("protocol.md")
    }

    pub fn prompt_md(&self) -> PathBuf {
        self.dir.join("prompt.md")
    }

    /// The mailbox. This is the directory granted to every session via `--add-dir`.
    pub fn briefs_dir(&self) -> PathBuf {
        self.dir.join("briefs")
    }

    pub fn brief_index(&self) -> PathBuf {
        self.briefs_dir().join("index.json")
    }

    pub fn brief_file(&self, filename: &str) -> PathBuf {
        self.briefs_dir().join(filename)
    }

    pub fn result_md(&self) -> PathBuf {
        self.dir.join("result.md")
    }

    pub fn result_json(&self) -> PathBuf {
        self.dir.join("result.json")
    }

    pub fn merge_json(&self) -> PathBuf {
        self.dir.join("merge.json")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.dir.join("logs")
    }

    /// The whole raw stream, so a failed task is fully reconstructable (§10). Deleted on merge
    /// per §8.4 — `logs/` alone can be tens of MB.
    pub fn session_log(&self, repo: &str) -> PathBuf {
        self.logs_dir().join(format!("{repo}.jsonl"))
    }
}

/// The user's home directory.
///
/// `$HOME` first, because that is what every shell, every test harness and every CI runner sets,
/// and honouring it is what makes the tool scriptable.
pub fn home_dir() -> Option<PathBuf> {
    if let Some(h) = std::env::var_os("HOME") {
        if !h.is_empty() {
            return Some(PathBuf::from(h));
        }
    }
    #[cfg(windows)]
    {
        if let Some(p) = std::env::var_os("USERPROFILE") {
            if !p.is_empty() {
                return Some(PathBuf::from(p));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_matches_section_8() {
        let p = Paths::at("/tmp/rp");
        assert_eq!(p.config_file(), PathBuf::from("/tmp/rp/config.toml"));
        assert_eq!(
            p.repo_cache_file(),
            PathBuf::from("/tmp/rp/cache/repos.json")
        );
        assert_eq!(p.log_file(), PathBuf::from("/tmp/rp/logs/reporch.log"));
        assert_eq!(
            p.project_base_json("example-app"),
            PathBuf::from("/tmp/rp/projects/example-app/project_base.json")
        );
        assert_eq!(
            p.repo_profile_json("example-app", "example-app-ui"),
            PathBuf::from("/tmp/rp/projects/example-app/repos/example-app-ui.json")
        );
    }

    #[test]
    fn task_dir_is_slug_then_id() {
        let p = Paths::at("/tmp/rp");
        assert_eq!(
            p.task_dir("example-app", "widget-status", TaskId::new(1)),
            PathBuf::from("/tmp/rp/projects/example-app/tasks/widget-status-TASK-0001")
        );
    }

    #[test]
    fn worktrees_are_central_and_siblings() {
        let p = Paths::at("/tmp/rp");
        let a = p.worktree_path("example-app", TaskId::new(1), "example-app");
        let b = p.worktree_path("example-app", TaskId::new(1), "example-app-ui");
        assert_eq!(
            a,
            PathBuf::from("/tmp/rp/worktrees/example-app/TASK-0001/example-app")
        );
        assert_eq!(
            a.parent(),
            b.parent(),
            "§9.1: repos stay siblings so ../example-app-ui keeps working"
        );
    }

    #[test]
    fn an_explicit_relative_root_is_anchored_before_other_commands_change_cwd() {
        let p = Paths::at("scratch/reporch-home");
        assert!(p.root().is_absolute(), "got {:?}", p.root());
        assert!(p.root().ends_with("scratch/reporch-home"));
    }

    #[test]
    fn task_paths_match_section_8() {
        let t = TaskPaths::at("/tmp/rp/projects/example-app/tasks/x-TASK-0001");
        assert!(t.task_json().ends_with("task.json"));
        assert!(t.base_md().ends_with("base.md"));
        assert!(t.protocol_md().ends_with("protocol.md"));
        assert!(t.brief_index().ends_with("briefs/index.json"));
        assert!(t
            .session_log("example-app")
            .ends_with("logs/example-app.jsonl"));
    }

    /// `REPORCH_HOME` overriding the location is an M1 acceptance criterion, and every spawned
    /// session depends on it (§7.1).
    #[test]
    fn reporch_home_overrides_the_default() {
        // Serialised via a mutex would be cleaner, but these two vars are only read here.
        let prev = std::env::var_os("REPORCH_HOME");
        // SAFETY-adjacent note: single-threaded test, restored below.
        std::env::set_var("REPORCH_HOME", "/custom/location");
        let p = Paths::resolve().unwrap();
        assert_eq!(p.root(), Path::new("/custom/location"));
        match prev {
            Some(v) => std::env::set_var("REPORCH_HOME", v),
            None => std::env::remove_var("REPORCH_HOME"),
        }
    }

    #[test]
    fn an_empty_reporch_home_falls_back_to_the_default() {
        let prev = std::env::var_os("REPORCH_HOME");
        std::env::set_var("REPORCH_HOME", "");
        let p = Paths::resolve().unwrap();
        assert!(
            p.root().ends_with(".reporch"),
            "empty must not resolve to the filesystem root: {:?}",
            p.root()
        );
        match prev {
            Some(v) => std::env::set_var("REPORCH_HOME", v),
            None => std::env::remove_var("REPORCH_HOME"),
        }
    }
}
