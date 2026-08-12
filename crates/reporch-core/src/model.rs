//! The persisted domain types: projects, tasks, repositories (§8).
//!
//! These are the serde shapes written to disk. Every one carries `schema_version`; adding a field
//! is fine, changing the meaning of one requires a migration (CLAUDE.md).
//!
//! Time is UTC RFC 3339 everywhere, carried as `String` here so this crate stays free of any
//! clock or I/O dependency. `reporch-store` owns the formatting.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

use crate::effort::{Effort, EffortPlan};
use crate::ids::TaskId;

/// A repo's role **for one task**. Elected per task; not a property of the project (§3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Master,
    /// The author's schema spells this `"standard"`; the English term used throughout the code is
    /// *worker*, which is unambiguous. The serialised value stays `"standard"` for compatibility
    /// (§3, §8.2).
    #[serde(rename = "standard")]
    Worker,
}

impl Role {
    pub const fn as_str(self) -> &'static str {
        match self {
            Role::Master => "master",
            Role::Worker => "worker",
        }
    }

    /// The value written to `task.json` and read back — the author's spelling (§8.2).
    pub const fn as_serialised(self) -> &'static str {
        match self {
            Role::Master => "master",
            Role::Worker => "standard",
        }
    }

    pub const fn is_master(self) -> bool {
        matches!(self, Role::Master)
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Task lifecycle (§8.2). The author's set, preserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Active,
    Completed,
    Merged,
    Cancelled,
    Delayed,
    Failed,
}

impl TaskStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Active => "active",
            TaskStatus::Completed => "completed",
            TaskStatus::Merged => "merged",
            TaskStatus::Cancelled => "cancelled",
            TaskStatus::Delayed => "delayed",
            TaskStatus::Failed => "failed",
        }
    }

    /// Whether the task is still running sessions.
    pub const fn is_running(self) -> bool {
        matches!(self, TaskStatus::Active)
    }

    /// Whether the task is finished and eligible for the merge flow (§11).
    pub const fn is_mergeable(self) -> bool {
        matches!(self, TaskStatus::Completed)
    }
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Per-repo session state, driving the run screen and the merge plan (§8.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RepoState {
    /// The session has been launched but has not yet emitted its first `system/init`.
    Spawning,
    /// Mid-turn: an injection has been made and no `result` has come back yet.
    Working,
    /// Idle with something outstanding — an unacked brief, or waiting on a peer.
    Waiting,
    /// The session reported itself blocked.
    Blocked,
    /// Finished its part.
    Done,
    /// The process exited: crash, or `terminal_reason: budget_exhausted` (§10 Deviation).
    Dead,
    /// Never briefed, never changed. Recorded `unchanged` in the result (§11.1).
    Idle,
}

impl RepoState {
    pub const fn as_str(self) -> &'static str {
        match self {
            RepoState::Spawning => "spawning",
            RepoState::Working => "working",
            RepoState::Waiting => "waiting",
            RepoState::Blocked => "blocked",
            RepoState::Done => "done",
            RepoState::Dead => "dead",
            RepoState::Idle => "idle",
        }
    }

    /// Whether reporch considers this session available to receive a ping.
    ///
    /// A dead session cannot be pinged; a working one does not need to be (an injection mid-turn
    /// is queued by Claude Code and runs as its own turn afterwards, but it is not *attention* —
    /// the session is already paying attention).
    ///
    /// **`Spawning` is pingable, and that is load-bearing.** `system/init` is emitted lazily, at
    /// the start of a session's first *turn* (spike #1) — not when the process starts. So a
    /// worker that has never been pinged has never emitted `init`, and sits in `Spawning`
    /// indefinitely. Treating that state as unpingable deadlocks the whole product: the workers
    /// cannot be woken because they have never been woken. Found by the first real end-to-end
    /// run, where the master correctly filed two briefs and neither recipient was ever told.
    pub const fn is_pingable(self) -> bool {
        matches!(
            self,
            RepoState::Waiting
                | RepoState::Idle
                | RepoState::Blocked
                | RepoState::Done
                | RepoState::Spawning
        )
    }

    /// Whether the session is quiet — i.e. not mid-turn.
    ///
    /// A freshly spawned session that has not been given a turn is quiet, for the same reason as
    /// above: if it were not, `is_quiescent_with_work` could never become true while any worker
    /// was still waiting for its first ping, and the §6.3 liveness sweep would never run.
    pub const fn is_quiet(self) -> bool {
        !matches!(self, RepoState::Working)
    }

    pub const fn is_terminal(self) -> bool {
        matches!(self, RepoState::Dead)
    }
}

impl fmt::Display for RepoState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Cumulative token accounting for one live Claude process.
///
/// Claude reports these totals in `result.modelUsage`; they are assigned from the latest result,
/// never summed turn by turn. A future resumed process starts a new cost/usage epoch (D036).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    #[serde(default)]
    pub input: u64,
    #[serde(default)]
    pub output: u64,
    #[serde(default)]
    pub cache_read: u64,
    #[serde(default)]
    pub cache_creation: u64,
}

impl TokenUsage {
    pub const fn is_zero(&self) -> bool {
        self.input == 0 && self.output == 0 && self.cache_read == 0 && self.cache_creation == 0
    }

    pub const fn saturating_add(self, other: Self) -> Self {
        Self {
            input: self.input.saturating_add(other.input),
            output: self.output.saturating_add(other.output),
            cache_read: self.cache_read.saturating_add(other.cache_read),
            cache_creation: self.cache_creation.saturating_add(other.cache_creation),
        }
    }
}

/// One repository's participation in one task (§8.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskRepo {
    pub name: String,
    /// The user's real checkout, never written to by a session.
    pub path: String,
    #[serde(rename = "task_role")]
    pub role: Role,
    pub worktree_path: String,
    pub branch: String,
    /// What the worktree was branched from. Merge is impossible without it (§8.2 note).
    pub base_commit: String,
    pub session_alias: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_pid: Option<u32>,
    pub model: String,
    pub effort: Effort,
    #[serde(default)]
    pub ultracode: bool,
    pub state: RepoState,
    /// `None` until the task finishes; then whether this repo produced a diff (§11.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<String>,
    /// Live spend for this process epoch, read from the latest `result.total_cost_usd`, which is
    /// cumulative within the child (§10 Deviation — never sum the per-turn values).
    #[serde(default)]
    pub spent_usd: f64,
    /// Latest cumulative `modelUsage` totals for this process epoch.
    #[serde(default, skip_serializing_if = "TokenUsage::is_zero")]
    pub token_usage: TokenUsage,
}

/// `task.json` (§8.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub schema_version: u32,
    pub id: TaskId,
    /// The slug. `org-scoped-api-keys`.
    pub name: String,
    pub status: TaskStatus,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    pub master_repo: String,
    pub model: String,
    pub master_effort: Effort,
    pub worker_effort: Effort,
    #[serde(default)]
    pub ultracode: bool,
    pub budget_usd: f64,
    #[serde(default)]
    pub spent_usd: f64,
    pub branch: String,
    pub repositories: Vec<TaskRepo>,
}

/// The current schema version for `task.json`.
pub const TASK_SCHEMA_VERSION: u32 = 1;

impl Task {
    pub fn repo(&self, name: &str) -> Option<&TaskRepo> {
        self.repositories.iter().find(|r| r.name == name)
    }

    pub fn repo_mut(&mut self, name: &str) -> Option<&mut TaskRepo> {
        self.repositories.iter_mut().find(|r| r.name == name)
    }

    pub fn master(&self) -> Option<&TaskRepo> {
        self.repositories.iter().find(|r| r.role.is_master())
    }

    pub fn workers(&self) -> impl Iterator<Item = &TaskRepo> {
        self.repositories.iter().filter(|r| !r.role.is_master())
    }

    /// The effort plan this task was created with.
    pub const fn effort_plan(&self) -> EffortPlan {
        EffortPlan {
            master: self.master_effort,
            worker: self.worker_effort,
            ultracode: self.ultracode,
        }
    }

    /// Total spend across every session. Each `TaskRepo.spent_usd` is already that session's
    /// cumulative total, so this *is* a sum — of sessions, not of turns.
    pub fn total_spent_usd(&self) -> f64 {
        self.repositories.iter().map(|r| r.spent_usd).sum()
    }

    /// Token totals across repository processes, never across turn snapshots.
    pub fn total_token_usage(&self) -> TokenUsage {
        self.repositories
            .iter()
            .fold(TokenUsage::default(), |sum, repo| {
                sum.saturating_add(repo.token_usage)
            })
    }

    /// Whether every repo has reached a state that allows `task finish` (§11.1): each is either
    /// `done`, or was never engaged and is therefore `unchanged`.
    pub fn all_repos_settled(&self) -> bool {
        self.repositories
            .iter()
            .all(|r| matches!(r.state, RepoState::Done | RepoState::Idle))
    }
}

/// One repo's analysis profile, from the §5.4 fan-out. Written to `repos/<name>.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RepoProfile {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub one_liner: String,
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default)]
    pub frameworks: Vec<String>,
    #[serde(default)]
    pub package_manager: Option<String>,
    #[serde(default)]
    pub entrypoints: Vec<String>,
    #[serde(default)]
    pub commands: BTreeMap<String, String>,
    #[serde(default)]
    pub deploy: Option<Deploy>,
    #[serde(default)]
    pub public_interfaces: Vec<String>,
    /// Repos whose contracts this one depends on. Half of the dependency graph (§5.4).
    #[serde(default)]
    pub consumes: Vec<String>,
    /// Repos that depend on this one's contracts. The other half.
    #[serde(default)]
    pub produces_contracts_for: Vec<String>,
    #[serde(default)]
    pub env_files: Vec<String>,
    /// Seeds `worktree_carry_over` (§9.2).
    #[serde(default)]
    pub untracked_needed_for_build: Vec<String>,
    #[serde(default)]
    pub confidence: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Deploy {
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub how: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

/// A repository as recorded in `project_base.json` (§8.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectRepo {
    pub name: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,
    #[serde(default)]
    pub purpose: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_ref: Option<String>,
    /// Set when the repo's own settings would refuse inbound peer messages (§4.4 Deviation).
    /// Advisory only — reporch pings over stdin, which this cannot block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cross_session_inbound: Option<String>,
}

/// How a file is carried into a fresh worktree (§9.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CarryMode {
    Copy,
    Symlink,
    /// APFS `clonefile` / Btrfs reflink, falling back to `Symlink` with a warning.
    CopyOnWrite,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CarryOver {
    pub pattern: String,
    pub mode: CarryMode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectSettings {
    #[serde(default = "default_permission_mode")]
    pub permission_mode: String,
    /// Set when the user confirms the §12.1 consent screen. `None` means no consent yet, and no
    /// task may start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consented_at: Option<String>,
    #[serde(default = "default_model")]
    pub default_model: String,
    #[serde(default = "default_budget")]
    pub max_budget_usd_per_session: f64,
    #[serde(default)]
    pub worktree_carry_over: Vec<CarryOver>,
    #[serde(default)]
    pub post_create_hooks: BTreeMap<String, Vec<String>>,
}

fn default_permission_mode() -> String {
    "bypassPermissions".to_string()
}
fn default_model() -> String {
    "opus".to_string()
}
fn default_budget() -> f64 {
    10.0
}

impl Default for ProjectSettings {
    fn default() -> Self {
        Self {
            permission_mode: default_permission_mode(),
            consented_at: None,
            default_model: default_model(),
            max_budget_usd_per_session: default_budget(),
            worktree_carry_over: Vec::new(),
            post_create_hooks: BTreeMap::new(),
        }
    }
}

impl ProjectSettings {
    /// Whether the §12.1 consent screen has been confirmed for this project. No task may start
    /// without it, and it is never a default.
    pub fn has_consent(&self) -> bool {
        self.consented_at.is_some()
    }
}

/// `project_base.json` (§8.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectBase {
    pub schema_version: u32,
    pub name: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analyzed_at: Option<String>,
    #[serde(default)]
    pub overview: String,
    pub repositories: Vec<ProjectRepo>,
    #[serde(default)]
    pub graph: BTreeMap<String, GraphNode>,
    #[serde(default)]
    pub settings: ProjectSettings,
    /// Allocated task IDs are monotonic per project.
    #[serde(default)]
    pub next_task_id: u32,
}

pub const PROJECT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GraphNode {
    #[serde(default)]
    pub consumes: Vec<String>,
    #[serde(default)]
    pub produces_contracts_for: Vec<String>,
}

impl ProjectBase {
    pub fn repo(&self, name: &str) -> Option<&ProjectRepo> {
        self.repositories.iter().find(|r| r.name == name)
    }

    pub fn repo_names(&self) -> Vec<String> {
        self.repositories.iter().map(|r| r.name.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_serialises_with_the_authors_spelling() {
        assert_eq!(serde_json::to_string(&Role::Master).unwrap(), "\"master\"");
        assert_eq!(
            serde_json::to_string(&Role::Worker).unwrap(),
            "\"standard\"",
            "§8.2 keeps the author's `standard` on the wire"
        );
        assert_eq!(
            serde_json::from_str::<Role>("\"standard\"").unwrap(),
            Role::Worker
        );
    }

    #[test]
    fn role_display_uses_the_english_term() {
        assert_eq!(Role::Worker.to_string(), "worker");
        assert_eq!(Role::Worker.as_serialised(), "standard");
    }

    #[test]
    fn repo_state_pingability() {
        // A session mid-turn already has attention; a dead one cannot receive any.
        assert!(!RepoState::Working.is_pingable());
        assert!(!RepoState::Dead.is_pingable());
        assert!(RepoState::Idle.is_pingable());
        assert!(RepoState::Waiting.is_pingable());
    }

    /// **Regression, from the first real end-to-end run.**
    ///
    /// `system/init` is lazy — it arrives at the start of a session's first turn, not when the
    /// process starts. A worker that has never been pinged therefore sits in `Spawning`. When
    /// that state was unpingable, the master filed two briefs and neither recipient was ever
    /// woken: the workers could not be pinged because they had never been pinged.
    #[test]
    fn a_freshly_spawned_session_can_be_woken() {
        assert!(
            RepoState::Spawning.is_pingable(),
            "a worker that has never taken a turn must still be reachable, or the task deadlocks"
        );
        assert!(
            RepoState::Spawning.is_quiet(),
            "or the §6.3 liveness sweep never runs while a worker awaits its first ping"
        );
    }

    #[test]
    fn quiet_is_the_complement_of_mid_turn() {
        for s in [
            RepoState::Spawning,
            RepoState::Working,
            RepoState::Waiting,
            RepoState::Blocked,
            RepoState::Done,
            RepoState::Dead,
            RepoState::Idle,
        ] {
            assert_eq!(s.is_quiet(), !matches!(s, RepoState::Working), "{s}");
        }
    }

    #[test]
    fn consent_is_never_a_default() {
        assert!(
            !ProjectSettings::default().has_consent(),
            "§12.1: consent is explicit, per project, never a default"
        );
    }

    #[test]
    fn task_status_gating() {
        assert!(TaskStatus::Active.is_running());
        assert!(!TaskStatus::Active.is_mergeable());
        assert!(TaskStatus::Completed.is_mergeable());
        assert!(!TaskStatus::Merged.is_mergeable(), "merge happens once");
    }
}
