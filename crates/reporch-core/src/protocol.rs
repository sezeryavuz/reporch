//! The orchestration protocol (§6), as a pure state machine.
//!
//! ```text
//! (TaskState, Event) -> (TaskState, Vec<Action>)
//! ```
//!
//! That signature is what makes the protocol testable: drive it with a recorded event log and
//! assert the ping decisions, with no Claude process and no filesystem in the loop (§13.3).
//!
//! # The rules (§6.1)
//!
//! | Actor | Write a brief to… | Ping… |
//! | :--- | :--- | :--- |
//! | **Master** | any worker, and itself | any worker |
//! | **Worker** | the master, and any other worker | **nobody** |
//! | **Control plane** | never | anyone, per §6.3 |
//!
//! # Why the control plane pings at all
//!
//! The author's rules are sound but contain a deadlock: a worker files a brief for the master and
//! stops. If the master is idle at that moment, nothing wakes it and the task hangs with everyone
//! waiting politely and nobody working. The control plane pings **on the worker's behalf**, under
//! narrow and logged conditions (§6.3). Workers still cannot ping.
//!
//! `TaskState::liveness_check` is the mechanised form of that guarantee, and the property test
//! `no_event_sequence_deadlocks` is why this design exists.
//!
//! # Time
//!
//! This crate has no clock. Every event carries `at_ms`, a monotonic millisecond timestamp
//! supplied by the caller. That keeps the machine pure and makes rate-limit tests exact rather
//! than flaky.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::brief::{BriefId, BriefKind, BriefStatus, PingBy};
use crate::model::{RepoState, Role, TokenUsage};

/// Guardrails on control-plane attention (§6.3). All of them mandatory.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PingLimits {
    /// At most one ping per session per this interval.
    pub min_interval_ms: u64,
    /// At most this many pings to any one session, for the whole task.
    pub max_per_session: u32,
    /// Global per-task ceiling. Exceeding it aborts the task and surfaces the loop to the user.
    pub max_per_task: u32,
    /// How long a worker may sit idle holding an unacked inbound brief before it is surfaced to
    /// the master's status (§6.3, row 4). Nobody is pinged for this.
    pub stale_inbound_ms: u64,
}

impl Default for PingLimits {
    fn default() -> Self {
        Self {
            min_interval_ms: 30_000,
            max_per_session: 20,
            max_per_task: 100,
            stale_inbound_ms: 180_000,
        }
    }
}

/// Something a session tried to do that the protocol forbids (§6.2).
///
/// Recorded rather than merely refused: the refusal happens at the CLI boundary, but a session
/// repeatedly attempting it is a signal worth surfacing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Violation {
    /// A worker invoked `reporch ping` (§6.2).
    WorkerAttemptedPing { repo: String, target: String },
    /// The master tried to ping itself, or a repo outside the task.
    PingTargetInvalid { by: String, target: String },
    /// A non-master invoked `reporch task finish` (§7.2).
    NonMasterAttemptedFinish { repo: String },
    /// A session's `system/init` still advertised `SendMessage`/`ListAgents` (§4.4 Deviation).
    /// This means the settings payload did not take effect and the protocol is unenforced.
    ToolDenyRulesNotApplied { repo: String, tools: Vec<String> },
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Violation::WorkerAttemptedPing { repo, target } => write!(
                f,
                "worker `{repo}` attempted to ping `{target}`; only the master delivers attention"
            ),
            Violation::PingTargetInvalid { by, target } => {
                write!(f, "`{by}` attempted to ping `{target}`, which is not a pingable repo in this task")
            }
            Violation::NonMasterAttemptedFinish { repo } => {
                write!(f, "`{repo}` attempted to finish the task; only the master may")
            }
            Violation::ToolDenyRulesNotApplied { repo, tools } => write!(
                f,
                "session for `{repo}` still has {} available — the deny rules did not apply and the protocol is unenforced",
                tools.join(", ")
            ),
        }
    }
}

/// Everything that can happen to a task.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A child process was launched for `repo`.
    SessionSpawned { repo: String, at_ms: u64 },
    /// The first `system/init` came back. `advertised_denied_tools` lists any of
    /// `SendMessage`/`ListAgents` still present — the §4.4 spawn assertion.
    SessionInitialised {
        repo: String,
        advertised_denied_tools: Vec<String>,
        at_ms: u64,
    },
    /// The session began a turn (first event after an injection).
    TurnStarted { repo: String, at_ms: u64 },
    /// A `result` message arrived. Cost and usage are process-cumulative snapshots, not this
    /// turn's increments (§10 Deviation).
    TurnEnded {
        repo: String,
        cumulative_cost_usd: f64,
        cumulative_usage: TokenUsage,
        at_ms: u64,
    },
    /// The child exited.
    SessionDied {
        repo: String,
        reason: DeathReason,
        at_ms: u64,
    },
    /// A brief was written to the mailbox by `from`.
    BriefFiled {
        id: BriefId,
        from: String,
        to: String,
        kind: BriefKind,
        at_ms: u64,
    },
    /// `reporch brief read <id>` was invoked by `by`.
    BriefRead { id: BriefId, by: String, at_ms: u64 },
    /// `reporch brief ack <id> --status …`.
    BriefAcked {
        id: BriefId,
        by: String,
        status: BriefStatus,
        at_ms: u64,
    },
    /// `reporch ping <repo>` was invoked by a session. Legal only from the master (§6.2).
    PingRequested {
        by: String,
        target: String,
        reason: Option<String>,
        at_ms: u64,
    },
    /// `reporch status --set …`.
    StatusSet {
        repo: String,
        state: RepoState,
        at_ms: u64,
    },
    /// `reporch task finish`. Legal only from the master.
    FinishRequested { by: String, at_ms: u64 },
    /// The periodic supervisor tick. This is what makes liveness happen.
    Tick { at_ms: u64 },
}

impl Event {
    pub const fn at_ms(&self) -> u64 {
        match self {
            Event::SessionSpawned { at_ms, .. }
            | Event::SessionInitialised { at_ms, .. }
            | Event::TurnStarted { at_ms, .. }
            | Event::TurnEnded { at_ms, .. }
            | Event::SessionDied { at_ms, .. }
            | Event::BriefFiled { at_ms, .. }
            | Event::BriefRead { at_ms, .. }
            | Event::BriefAcked { at_ms, .. }
            | Event::PingRequested { at_ms, .. }
            | Event::StatusSet { at_ms, .. }
            | Event::FinishRequested { at_ms, .. }
            | Event::Tick { at_ms } => *at_ms,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeathReason {
    /// `terminal_reason: "budget_exhausted"` — the process exits, it does not pause (§10).
    BudgetExhausted,
    Crashed,
    /// reporch terminated it deliberately.
    TornDown,
}

/// What the supervisor must do as a result of a transition.
///
/// Actions are *descriptions*, never effects — that is what keeps this crate pure.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Write a line to a session's stdin (§6.4).
    DeliverPing {
        to: String,
        body: String,
        by: PingBy,
        reason: String,
        at_ms: u64,
    },
    /// `task.json` changed.
    PersistTask,
    /// A protocol violation to log loudly and surface.
    RecordViolation(Violation),
    /// Something needs attention but no ping may be sent — surfaced to the user rather than
    /// silently swallowed. This is what stops a rate limit from becoming a deadlock.
    Surface { message: String },
    /// The global ping ceiling was exceeded: a loop. Abort and show it (§6.3).
    AbortTask { reason: String },
    /// Every repo has settled and the master asked to finish (§11.1).
    CompleteTask,
}

/// Why a ping was or was not delivered. Returned by the decision helper so tests can assert on
/// intent, not just on the emitted action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PingDecision {
    Allowed,
    /// Refused because the caller is not permitted to ping (§6.1).
    NotPermitted,
    /// Within `min_interval_ms` of the previous ping to this session.
    TooSoon {
        retry_after_ms: u64,
    },
    /// This session has already received `max_per_session` pings.
    SessionCeiling,
    /// The task has already delivered `max_per_task` pings.
    TaskCeiling,
    /// Identical to the previous ping body for this session (§6.3).
    Duplicate,
    /// The target is mid-turn or dead — it does not need, or cannot receive, attention.
    NotPingable,
}

impl PingDecision {
    pub const fn is_allowed(&self) -> bool {
        matches!(self, PingDecision::Allowed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProtocolError {
    #[error("`{0}` is not a repository in this task")]
    UnknownRepo(String),
    #[error("this task has no master repo")]
    NoMaster,
}

/// Per-session runtime state.
#[derive(Debug, Clone, PartialEq)]
struct RepoRuntime {
    role: Role,
    state: RepoState,
    /// Cumulative session spend, straight from the latest `result.total_cost_usd`.
    spent_usd: f64,
    /// Cumulative process tokens, summed across `result.modelUsage` models.
    token_usage: TokenUsage,
    pings_received: u32,
    last_ping_at_ms: Option<u64>,
    last_ping_body: Option<String>,
    last_activity_at_ms: u64,
    /// Set when this repo has been surfaced as stalled, so it is surfaced once, not every tick.
    stall_surfaced: bool,
}

/// One outstanding brief, as the protocol sees it. The full content lives on disk.
#[derive(Debug, Clone, PartialEq)]
struct BriefRuntime {
    from: String,
    to: String,
    kind: BriefKind,
    status: BriefStatus,
    filed_at_ms: u64,
}

/// The live state of one task.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskState {
    master: String,
    repos: BTreeMap<String, RepoRuntime>,
    briefs: BTreeMap<BriefId, BriefRuntime>,
    limits: PingLimits,
    pings_delivered: u32,
    aborted: bool,
    completed: bool,
    /// When the "no ping may be delivered" stall was last surfaced.
    ///
    /// A stalled task must keep saying so — a notice shown once and never again is as good as
    /// silence when the user looks away — but not on every tick, which fills the run screen with
    /// one repeated line. It re-surfaces on the same interval that bounds pings.
    stall_surfaced_at_ms: Option<u64>,
    violations: Vec<Violation>,
    now_ms: u64,
}

impl TaskState {
    /// Create the state for a task that is about to start.
    ///
    /// `repos` is every repo in the project, with the elected master flagged.
    pub fn new(master: impl Into<String>, repos: &[(String, Role)], limits: PingLimits) -> Self {
        let master = master.into();
        let repos = repos
            .iter()
            .map(|(name, role)| {
                (
                    name.clone(),
                    RepoRuntime {
                        role: *role,
                        state: RepoState::Spawning,
                        spent_usd: 0.0,
                        token_usage: TokenUsage::default(),
                        pings_received: 0,
                        last_ping_at_ms: None,
                        last_ping_body: None,
                        last_activity_at_ms: 0,
                        stall_surfaced: false,
                    },
                )
            })
            .collect();
        Self {
            master,
            repos,
            briefs: BTreeMap::new(),
            limits,
            pings_delivered: 0,
            aborted: false,
            completed: false,
            stall_surfaced_at_ms: None,
            violations: Vec::new(),
            now_ms: 0,
        }
    }

    pub fn master(&self) -> &str {
        &self.master
    }

    pub fn limits(&self) -> PingLimits {
        self.limits
    }

    pub fn is_aborted(&self) -> bool {
        self.aborted
    }

    pub fn is_completed(&self) -> bool {
        self.completed
    }

    pub fn pings_delivered(&self) -> u32 {
        self.pings_delivered
    }

    pub fn violations(&self) -> &[Violation] {
        &self.violations
    }

    pub fn state_of(&self, repo: &str) -> Option<RepoState> {
        self.repos.get(repo).map(|r| r.state)
    }

    pub fn role_of(&self, repo: &str) -> Option<Role> {
        self.repos.get(repo).map(|r| r.role)
    }

    pub fn spent_usd(&self, repo: &str) -> f64 {
        self.repos.get(repo).map_or(0.0, |r| r.spent_usd)
    }

    pub fn token_usage(&self, repo: &str) -> TokenUsage {
        self.repos
            .get(repo)
            .map_or_else(TokenUsage::default, |r| r.token_usage)
    }

    /// Total spend across every session. Each session's figure is already cumulative, so this is
    /// a sum over sessions, never over turns (§10 Deviation).
    pub fn total_spent_usd(&self) -> f64 {
        self.repos.values().map(|r| r.spent_usd).sum()
    }

    pub fn total_token_usage(&self) -> TokenUsage {
        self.repos
            .values()
            .fold(TokenUsage::default(), |sum, repo| {
                sum.saturating_add(repo.token_usage)
            })
    }

    pub fn pings_received(&self, repo: &str) -> u32 {
        self.repos.get(repo).map_or(0, |r| r.pings_received)
    }

    /// Briefs addressed to `repo` that are not closed out.
    pub fn outstanding_for(&self, repo: &str) -> usize {
        self.briefs
            .values()
            .filter(|b| b.to == repo && !b.status.is_closed())
            .count()
    }

    pub fn unread_for(&self, repo: &str) -> usize {
        self.briefs
            .values()
            .filter(|b| b.to == repo && b.status == BriefStatus::Unread)
            .count()
    }

    /// Briefs `repo` sent that nobody has closed out yet.
    pub fn open_outbound(&self, repo: &str) -> usize {
        self.briefs
            .values()
            .filter(|b| b.from == repo && !b.status.is_closed())
            .count()
    }

    /// Whether every session is quiet — nobody is mid-turn.
    pub fn all_quiet(&self) -> bool {
        self.repos.values().all(|r| r.state.is_quiet())
    }

    /// Whether any brief anywhere is still outstanding.
    pub fn any_outstanding_briefs(&self) -> bool {
        self.briefs.values().any(|b| !b.status.is_closed())
    }

    /// Whether every worker has either completed its part or was never involved, leaving only
    /// the master's result report and explicit finish command.
    ///
    /// A never-briefed eager child remains [`RepoState::Spawning`] because `system/init` and the
    /// paid startup charge are both lazy until its first turn. That state is unchanged work, not
    /// an unsettled worker. This is still work overall: without a final control-plane nudge an
    /// idle master can sleep forever after the last acknowledgement.
    pub fn ready_for_master_completion(&self) -> bool {
        !self.aborted
            && !self.completed
            && !self.any_outstanding_briefs()
            && self
                .repos
                .iter()
                .filter(|(name, _)| **name != self.master)
                .all(|(name, repo)| {
                    repo.state == RepoState::Done
                        || (matches!(repo.state, RepoState::Idle | RepoState::Spawning)
                            && !self
                                .briefs
                                .values()
                                .any(|brief| &brief.to == name || &brief.from == name))
                })
    }

    /// **The deadlock condition.** Every session quiet, and work still outstanding.
    ///
    /// Reaching this state is fine and normal — it happens between turns. *Staying* in it is the
    /// failure §6.3 exists to prevent, and [`TaskState::step`] guarantees that a `Tick` in this
    /// state always produces an [`Action`]: a ping, a surfaced stall, or an abort.
    pub fn is_quiescent_with_work(&self) -> bool {
        !self.aborted
            && !self.completed
            && self.all_quiet()
            && (self.any_outstanding_briefs() || self.ready_for_master_completion())
            && self.repos.values().any(|r| !r.state.is_terminal())
    }

    // ---------------------------------------------------------------- transitions

    /// The transition. Pure: same state plus same event always yields the same result.
    #[must_use = "the caller must perform the returned actions"]
    pub fn step(mut self, event: Event) -> (Self, Vec<Action>) {
        self.now_ms = self.now_ms.max(event.at_ms());
        // Anything other than a bare tick is progress of some kind, so the stall notice becomes
        // relevant again immediately.
        if !matches!(event, Event::Tick { .. }) {
            self.stall_surfaced_at_ms = None;
        }
        let mut actions = Vec::new();
        if self.aborted || self.completed {
            // A finished task absorbs further events without acting on them.
            return (self, actions);
        }

        match event {
            Event::SessionSpawned { repo, at_ms } => {
                if let Some(r) = self.repos.get_mut(&repo) {
                    r.state = RepoState::Spawning;
                    r.last_activity_at_ms = at_ms;
                    actions.push(Action::PersistTask);
                }
            }

            Event::SessionInitialised {
                repo,
                advertised_denied_tools,
                at_ms,
            } => {
                // §4.4 Deviation: the spawn assertion. `-p` silently ignores a settings payload
                // that fails validation, so the flag having been passed proves nothing. If the
                // tools are still there, the protocol is unenforced and the task must not run.
                if !advertised_denied_tools.is_empty() {
                    let v = Violation::ToolDenyRulesNotApplied {
                        repo: repo.clone(),
                        tools: advertised_denied_tools,
                    };
                    self.violations.push(v.clone());
                    actions.push(Action::RecordViolation(v));
                    self.aborted = true;
                    actions.push(Action::AbortTask {
                        reason: format!(
                            "session for `{repo}` did not have SendMessage/ListAgents removed; \
                             the protocol cannot be enforced"
                        ),
                    });
                    return (self, actions);
                }
                if let Some(r) = self.repos.get_mut(&repo) {
                    if r.state == RepoState::Spawning {
                        r.state = RepoState::Idle;
                    }
                    r.last_activity_at_ms = at_ms;
                    actions.push(Action::PersistTask);
                }
            }

            Event::TurnStarted { repo, at_ms } => {
                if let Some(r) = self.repos.get_mut(&repo) {
                    r.state = RepoState::Working;
                    r.last_activity_at_ms = at_ms;
                    r.stall_surfaced = false;
                    actions.push(Action::PersistTask);
                }
            }

            Event::TurnEnded {
                repo,
                cumulative_cost_usd,
                cumulative_usage,
                at_ms,
            } => {
                if let Some(r) = self.repos.get_mut(&repo) {
                    // Assignment, not addition: the figure is already cumulative.
                    r.spent_usd = cumulative_cost_usd;
                    r.token_usage = cumulative_usage;
                    r.last_activity_at_ms = at_ms;
                    if r.state == RepoState::Working {
                        r.state = if self
                            .briefs
                            .values()
                            .any(|b| b.to == repo && !b.status.is_closed())
                        {
                            RepoState::Waiting
                        } else {
                            RepoState::Idle
                        };
                    }
                    actions.push(Action::PersistTask);
                }
            }

            Event::SessionDied {
                repo,
                reason,
                at_ms,
            } => {
                if let Some(r) = self.repos.get_mut(&repo) {
                    r.state = RepoState::Dead;
                    r.last_activity_at_ms = at_ms;
                }
                actions.push(Action::PersistTask);
                let detail = match reason {
                    DeathReason::BudgetExhausted => {
                        "hit its --max-budget-usd ceiling and exited. Raise the ceiling and resume \
                         to continue."
                    }
                    DeathReason::Crashed => "exited unexpectedly.",
                    DeathReason::TornDown => "was terminated by reporch.",
                };
                if !matches!(reason, DeathReason::TornDown) {
                    actions.push(Action::Surface {
                        message: format!("session for `{repo}` {detail}"),
                    });
                }
            }

            Event::StatusSet { repo, state, at_ms } => {
                if let Some(r) = self.repos.get_mut(&repo) {
                    r.state = state;
                    r.last_activity_at_ms = at_ms;
                    r.stall_surfaced = false;
                    actions.push(Action::PersistTask);
                }
            }

            Event::BriefFiled {
                id,
                from,
                to,
                kind,
                at_ms,
            } => {
                self.briefs.insert(
                    id,
                    BriefRuntime {
                        from: from.clone(),
                        to: to.clone(),
                        kind,
                        status: BriefStatus::Unread,
                        filed_at_ms: at_ms,
                    },
                );
                actions.push(Action::PersistTask);

                // §6.3 rows 1 and 2: a brief landing for a quiet session earns a nudge.
                // Note the asymmetry, which is the whole design: the *master* pinging a worker is
                // the master's own privilege exercised through the control plane; a worker's brief
                // to the master is nudged on the worker's behalf, because workers cannot ping.
                if from != to {
                    let body = if to == self.master {
                        let n = self.unread_for(&self.master);
                        format!(
                            "You have {n} unread brief{}. Read {} with `reporch brief inbox`.",
                            if n == 1 { "" } else { "s" },
                            if n == 1 { "it" } else { "them" }
                        )
                    } else {
                        format!(
                            "Brief {id} from `{from}` is waiting. Read it with \
                             `reporch brief read {id}` and act."
                        )
                    };
                    let reason = format!("brief {id} filed for {to}");
                    self.try_ping(&to, body, PingBy::ControlPlane, reason, at_ms, &mut actions);
                }
            }

            Event::BriefRead { id, by, at_ms } => {
                if let Some(b) = self.briefs.get_mut(&id) {
                    if b.status == BriefStatus::Unread && b.to == by {
                        b.status = BriefStatus::Read;
                        actions.push(Action::PersistTask);
                    }
                }
                if let Some(r) = self.repos.get_mut(&by) {
                    r.last_activity_at_ms = at_ms;
                    r.stall_surfaced = false;
                }
            }

            Event::BriefAcked {
                id,
                by,
                status,
                at_ms,
            } => {
                if let Some(b) = self.briefs.get_mut(&id) {
                    if b.to == by && b.status.can_transition_to(status) {
                        b.status = status;
                        actions.push(Action::PersistTask);
                    }
                }
                if let Some(r) = self.repos.get_mut(&by) {
                    r.last_activity_at_ms = at_ms;
                    r.stall_surfaced = false;
                    // Once nothing is outstanding for this repo, it drops back to idle.
                    if r.state == RepoState::Waiting
                        && !self
                            .briefs
                            .values()
                            .any(|b| b.to == by && !b.status.is_closed())
                    {
                        r.state = RepoState::Idle;
                    }
                }
            }

            Event::PingRequested {
                by,
                target,
                reason,
                at_ms,
            } => {
                // §6.2, mechanism 2. The CLI refuses this before it ever reaches here, but the
                // state machine refuses it too: enforcement must not depend on one layer.
                match self.repos.get(&by).map(|r| r.role) {
                    Some(Role::Master) => {}
                    Some(Role::Worker) => {
                        let v = Violation::WorkerAttemptedPing {
                            repo: by.clone(),
                            target: target.clone(),
                        };
                        self.violations.push(v.clone());
                        actions.push(Action::RecordViolation(v));
                        return (self, actions);
                    }
                    None => {
                        let v = Violation::PingTargetInvalid {
                            by: by.clone(),
                            target: target.clone(),
                        };
                        self.violations.push(v.clone());
                        actions.push(Action::RecordViolation(v));
                        return (self, actions);
                    }
                }
                // The master may ping any worker — not itself, and not a repo outside the task.
                if target == by || !self.repos.contains_key(&target) {
                    let v = Violation::PingTargetInvalid {
                        by: by.clone(),
                        target: target.clone(),
                    };
                    self.violations.push(v.clone());
                    actions.push(Action::RecordViolation(v));
                    return (self, actions);
                }
                let reason = reason.unwrap_or_else(|| format!("requested by {by}"));
                let n = self.unread_for(&target);
                let body = if n > 0 {
                    format!(
                        "`{by}` asks you to pick this up. You have {n} unread brief{}. \
                         Read with `reporch brief inbox`.",
                        if n == 1 { "" } else { "s" }
                    )
                } else {
                    format!("`{by}` asks you to check in: {reason}")
                };
                self.try_ping(&target, body, PingBy::Master, reason, at_ms, &mut actions);
            }

            Event::FinishRequested { by, at_ms: _ } => {
                if self.repos.get(&by).map(|r| r.role) != Some(Role::Master) {
                    let v = Violation::NonMasterAttemptedFinish { repo: by.clone() };
                    self.violations.push(v.clone());
                    actions.push(Action::RecordViolation(v));
                    return (self, actions);
                }
                self.completed = true;
                actions.push(Action::PersistTask);
                actions.push(Action::CompleteTask);
            }

            Event::Tick { at_ms } => {
                self.liveness_check(at_ms, &mut actions);
            }
        }

        (self, actions)
    }

    /// The periodic liveness sweep (§6.3), and the guarantee that quiescence is never permanent.
    ///
    /// Called on every `Tick`. If the task is quiescent with work outstanding, this **must**
    /// produce an action — a ping, a surfaced stall, or an abort.
    fn liveness_check(&mut self, at_ms: u64, actions: &mut Vec<Action>) {
        // Row 4: a worker idle too long holding an unacked inbound brief. Nobody is pinged; it is
        // surfaced to the master's status and the master decides.
        let stale: Vec<String> = self
            .repos
            .iter()
            .filter(|(name, r)| {
                **name != self.master
                    && r.state.is_quiet()
                    && !r.state.is_terminal()
                    && !r.stall_surfaced
                    && self.briefs.values().any(|b| {
                        &b.to == *name
                            && !b.status.is_closed()
                            && at_ms.saturating_sub(b.filed_at_ms) > self.limits.stale_inbound_ms
                    })
            })
            .map(|(n, _)| n.clone())
            .collect();
        for name in stale {
            if let Some(r) = self.repos.get_mut(&name) {
                r.stall_surfaced = true;
            }
            actions.push(Action::Surface {
                message: format!(
                    "`{name}` has been idle with an unacked brief for over {}s. \
                     `reporch ping {name}` to re-prompt it.",
                    self.limits.stale_inbound_ms / 1000
                ),
            });
        }

        if !self.is_quiescent_with_work() {
            return;
        }

        // Rows 1 and 2, retried: any quiet session holding unread briefs.
        // Sorted for determinism; the master goes first because it is the one that unblocks
        // everyone else.
        let mut targets: Vec<String> = self
            .repos
            .iter()
            .filter(|(name, r)| r.state.is_pingable() && self.unread_for(name) > 0)
            .map(|(n, _)| n.clone())
            .collect();
        targets.sort_by_key(|n| (*n != self.master, n.clone()));

        let mut acted = !actions.is_empty();
        for name in targets {
            let n = self.unread_for(&name);
            let body = if name == self.master {
                format!(
                    "You have {n} unread brief{}. Read {} with `reporch brief inbox`.",
                    if n == 1 { "" } else { "s" },
                    if n == 1 { "it" } else { "them" }
                )
            } else {
                format!(
                    "You have {n} unread brief{}. Read {} with `reporch brief inbox` and act.",
                    if n == 1 { "" } else { "s" },
                    if n == 1 { "it" } else { "them" }
                )
            };
            let before = actions.len();
            self.try_ping(
                &name,
                body,
                PingBy::ControlPlane,
                "unread inbox while idle".to_string(),
                at_ms,
                actions,
            );
            if actions.len() > before {
                acted = true;
            }
            if self.aborted {
                return;
            }
        }

        // Row 3: the master is idle, its inbox is empty, and it has open outbound briefs — it is
        // waiting on repos that have not replied.
        //
        // Gated on nothing else having fired this tick. If a worker was just woken, the master
        // will hear back from it and does not need telling that it is waiting — row 3 is for a
        // task that has genuinely stalled, not for one where work was just dispatched.
        let master_idle_waiting = !acted
            && self
                .repos
                .get(&self.master)
                .is_some_and(|r| r.state.is_pingable())
            && self.unread_for(&self.master) == 0
            && self.briefs.values().any(|brief| {
                brief.from == self.master
                    && !brief.status.is_closed()
                    // A brief that was filed on this supervisor tick is dispatch, not a stall.
                    // Give its worker one normal attention interval before reminding the master.
                    && at_ms.saturating_sub(brief.filed_at_ms) >= self.limits.min_interval_ms
            });
        if master_idle_waiting {
            let n = self.open_outbound(&self.master);
            let before = actions.len();
            let master = self.master.clone();
            self.try_ping(
                &master,
                format!(
                    "You are waiting on {n} repo{}. `reporch brief inbox` to check; \
                     `reporch ping <repo>` to re-prompt.",
                    if n == 1 { "" } else { "s" }
                ),
                PingBy::ControlPlane,
                "idle with open outbound briefs".to_string(),
                at_ms,
                actions,
            );
            if actions.len() > before {
                acted = true;
            }
        }

        // Completion handoff: workers are instructed to acknowledge their inbound brief, set
        // their own status to done, and stop. An acknowledgement is not a reply brief, so when
        // the final worker settles there is otherwise no event addressed to the idle master.
        if !acted
            && self.ready_for_master_completion()
            && self
                .repos
                .get(&self.master)
                .is_some_and(|r| r.state.is_pingable())
        {
            let before = actions.len();
            let master = self.master.clone();
            self.try_ping(
                &master,
                "Every worker repository is done or unchanged and no briefs remain. Review \
                 `reporch repos` and the mailbox, write `result.md`, then run \
                 `reporch task finish --report-file result.md`."
                    .to_string(),
                PingBy::ControlPlane,
                "all workers settled; result report pending".to_string(),
                at_ms,
                actions,
            );
            if actions.len() > before {
                acted = true;
            }
        }

        // The guarantee. If work is outstanding, everyone is quiet, and nothing above produced an
        // action, the task is stuck in a way the user must see. Rate limits delay attention; they
        // must never silently absorb it.
        let stall_due = self
            .stall_surfaced_at_ms
            .is_none_or(|then| at_ms.saturating_sub(then) >= self.limits.min_interval_ms);
        if !acted && self.is_quiescent_with_work() && stall_due {
            let blocked: Vec<String> = self
                .repos
                .iter()
                .filter(|(n, _)| self.outstanding_for(n) > 0)
                .map(|(n, r)| format!("{n} ({} pings so far)", r.pings_received))
                .collect();
            if !blocked.is_empty() {
                self.stall_surfaced_at_ms = Some(at_ms);
                actions.push(Action::Surface {
                    message: format!(
                        "Task is idle with unfinished briefs and no ping may be delivered: {}. \
                         Attention limits have been reached — intervene, or abort.",
                        blocked.join(", ")
                    ),
                });
            } else if self.ready_for_master_completion() {
                self.stall_surfaced_at_ms = Some(at_ms);
                actions.push(Action::Surface {
                    message: "Every worker is done or unchanged, but the master has not finished \
                              the task and no completion ping may be delivered. Inspect the \
                              master session and result report."
                        .to_string(),
                });
            }
        }
    }

    /// Evaluate the §6.3 guardrails without mutating anything.
    pub fn ping_decision(&self, target: &str, body: &str, at_ms: u64) -> PingDecision {
        let Some(r) = self.repos.get(target) else {
            return PingDecision::NotPermitted;
        };
        if !r.state.is_pingable() {
            return PingDecision::NotPingable;
        }
        if self.pings_delivered >= self.limits.max_per_task {
            return PingDecision::TaskCeiling;
        }
        if r.pings_received >= self.limits.max_per_session {
            return PingDecision::SessionCeiling;
        }
        if let Some(last) = r.last_ping_at_ms {
            let elapsed = at_ms.saturating_sub(last);
            if elapsed < self.limits.min_interval_ms {
                return PingDecision::TooSoon {
                    retry_after_ms: self.limits.min_interval_ms - elapsed,
                };
            }
        }
        // Identical consecutive ping bodies are dropped (§6.3).
        if r.last_ping_body.as_deref() == Some(body) {
            return PingDecision::Duplicate;
        }
        PingDecision::Allowed
    }

    /// Apply the guardrails and, if they allow it, emit the ping.
    fn try_ping(
        &mut self,
        target: &str,
        body: String,
        by: PingBy,
        reason: String,
        at_ms: u64,
        actions: &mut Vec<Action>,
    ) {
        match self.ping_decision(target, &body, at_ms) {
            PingDecision::Allowed => {}
            PingDecision::TaskCeiling => {
                // A global ceiling breach is a loop. Abort and surface it (§6.3).
                self.aborted = true;
                actions.push(Action::AbortTask {
                    reason: format!(
                        "ping ceiling of {} for this task was reached — this is a loop, not \
                         progress. Inspect the mailbox with `reporch brief inbox`.",
                        self.limits.max_per_task
                    ),
                });
                return;
            }
            PingDecision::SessionCeiling => {
                if let Some(r) = self.repos.get_mut(target) {
                    if !r.stall_surfaced {
                        r.stall_surfaced = true;
                        actions.push(Action::Surface {
                            message: format!(
                                "`{target}` has received the maximum of {} pings for this task \
                                 and is still not finished. It may be stuck.",
                                self.limits.max_per_session
                            ),
                        });
                    }
                }
                return;
            }
            // Not errors: a later tick retries.
            PingDecision::TooSoon { .. }
            | PingDecision::Duplicate
            | PingDecision::NotPingable
            | PingDecision::NotPermitted => return,
        }

        let r = self
            .repos
            .get_mut(target)
            .expect("ping_decision returned Allowed for a repo that exists");
        r.pings_received += 1;
        r.last_ping_at_ms = Some(at_ms);
        r.last_ping_body = Some(body.clone());
        self.pings_delivered += 1;
        actions.push(Action::DeliverPing {
            to: target.to_string(),
            body,
            by,
            reason,
            at_ms,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: &str = "example-app";
    const UI: &str = "example-app-ui";
    const JS: &str = "example-app-js";

    fn repos() -> Vec<(String, Role)> {
        vec![
            (M.to_string(), Role::Master),
            (UI.to_string(), Role::Worker),
            (JS.to_string(), Role::Worker),
        ]
    }

    fn state() -> TaskState {
        TaskState::new(M, &repos(), PingLimits::default())
    }

    /// Bring every session up and idle, as the supervisor would.
    fn started() -> TaskState {
        let mut s = state();
        for (name, _) in repos() {
            let (ns, _) = s.step(Event::SessionInitialised {
                repo: name,
                advertised_denied_tools: vec![],
                at_ms: 0,
            });
            s = ns;
        }
        s
    }

    fn pings(actions: &[Action]) -> Vec<(&str, &str)> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::DeliverPing { to, body, .. } => Some((to.as_str(), body.as_str())),
                _ => None,
            })
            .collect()
    }

    // ---------------------------------------------------------- §6.1, the rules

    #[test]
    fn a_worker_can_never_produce_a_ping() {
        let s = started();
        let (s, actions) = s.step(Event::PingRequested {
            by: UI.to_string(),
            target: JS.to_string(),
            reason: Some("please".into()),
            at_ms: 1000,
        });
        assert!(pings(&actions).is_empty(), "workers never ping");
        assert_eq!(
            s.violations(),
            &[Violation::WorkerAttemptedPing {
                repo: UI.to_string(),
                target: JS.to_string()
            }]
        );
        assert!(actions
            .iter()
            .any(|a| matches!(a, Action::RecordViolation(_))));
    }

    #[test]
    fn the_master_may_ping_a_worker() {
        let s = started();
        let (_, actions) = s.step(Event::PingRequested {
            by: M.to_string(),
            target: UI.to_string(),
            reason: Some("brief 0007".into()),
            at_ms: 1000,
        });
        let p = pings(&actions);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].0, UI);
    }

    #[test]
    fn the_master_may_not_ping_itself_or_an_unknown_repo() {
        for target in [M, "not-in-this-task"] {
            let s = started();
            let (s, actions) = s.step(Event::PingRequested {
                by: M.to_string(),
                target: target.to_string(),
                reason: None,
                at_ms: 1000,
            });
            assert!(pings(&actions).is_empty(), "target {target}");
            assert!(matches!(
                s.violations().first(),
                Some(Violation::PingTargetInvalid { .. })
            ));
        }
    }

    #[test]
    fn only_the_master_may_finish_the_task() {
        let s = started();
        let (s, actions) = s.step(Event::FinishRequested {
            by: UI.to_string(),
            at_ms: 1,
        });
        assert!(!s.is_completed());
        assert!(matches!(
            s.violations().first(),
            Some(Violation::NonMasterAttemptedFinish { .. })
        ));
        assert!(!actions.iter().any(|a| matches!(a, Action::CompleteTask)));

        let s = started();
        let (s, actions) = s.step(Event::FinishRequested {
            by: M.to_string(),
            at_ms: 1,
        });
        assert!(s.is_completed());
        assert!(actions.iter().any(|a| matches!(a, Action::CompleteTask)));
    }

    // ------------------------------------------------- §6.3, the control plane

    #[test]
    fn a_brief_for_an_idle_master_produces_exactly_one_ping() {
        let s = started();
        let (_, actions) = s.step(Event::BriefFiled {
            id: BriefId::new(1),
            from: UI.to_string(),
            to: M.to_string(),
            kind: BriefKind::Reply,
            at_ms: 1000,
        });
        let p = pings(&actions);
        assert_eq!(p.len(), 1, "exactly one ping");
        assert_eq!(p[0].0, M);
        assert!(p[0].1.contains("1 unread brief"));
        assert!(p[0].1.contains("reporch brief inbox"));
    }

    #[test]
    fn a_brief_for_a_busy_master_produces_no_ping() {
        // The master is mid-turn, so it will see the brief on its own schedule (Appendix A, 8).
        let s = started();
        let (s, _) = s.step(Event::TurnStarted {
            repo: M.to_string(),
            at_ms: 500,
        });
        let (_, actions) = s.step(Event::BriefFiled {
            id: BriefId::new(1),
            from: UI.to_string(),
            to: M.to_string(),
            kind: BriefKind::Reply,
            at_ms: 1000,
        });
        assert!(pings(&actions).is_empty());
    }

    #[test]
    fn a_brief_for_an_idle_worker_is_nudged_with_the_read_command() {
        let s = started();
        let (_, actions) = s.step(Event::BriefFiled {
            id: BriefId::new(3),
            from: M.to_string(),
            to: UI.to_string(),
            kind: BriefKind::Request,
            at_ms: 1000,
        });
        let p = pings(&actions);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].0, UI);
        assert!(
            p[0].1.contains("reporch brief read 0003"),
            "got: {}",
            p[0].1
        );
    }

    #[test]
    fn a_master_idle_with_open_outbound_briefs_is_reminded() {
        // The master dispatched work and went quiet; the worker picked the brief up (so no
        // arrival nudge is due) but has not reported back. §6.3 row 3 is what unsticks this.
        let s = started();
        let (s, _) = s.step(Event::BriefFiled {
            id: BriefId::new(1),
            from: M.to_string(),
            to: UI.to_string(),
            kind: BriefKind::Request,
            at_ms: 0,
        });
        let (s, _) = s.step(Event::BriefRead {
            id: BriefId::new(1),
            by: UI.to_string(),
            at_ms: 1_000,
        });
        // Well past the rate-limit interval, and short of the row-4 stale threshold.
        let (_, actions) = s.step(Event::Tick { at_ms: 120_000 });
        let p = pings(&actions);
        assert_eq!(p.len(), 1, "only the master is reminded; got {p:?}");
        assert_eq!(p[0].0, M);
        assert!(p[0].1.contains("waiting on 1 repo"), "got: {}", p[0].1);
    }

    #[test]
    fn a_master_is_woken_to_finish_after_every_worker_settles() {
        let s = started();
        let (s, _) = s.step(Event::BriefFiled {
            id: BriefId::new(1),
            from: M.to_string(),
            to: UI.to_string(),
            kind: BriefKind::Request,
            at_ms: 0,
        });
        let (s, _) = s.step(Event::BriefAcked {
            id: BriefId::new(1),
            by: UI.to_string(),
            status: BriefStatus::Done,
            at_ms: 1_000,
        });
        let (s, _) = s.step(Event::StatusSet {
            repo: UI.to_string(),
            state: RepoState::Done,
            at_ms: 1_000,
        });

        let (_, actions) = s.step(Event::Tick { at_ms: 31_000 });
        let p = pings(&actions);
        assert_eq!(
            p.len(),
            1,
            "the master must not sleep past completion: {p:?}"
        );
        assert_eq!(p[0].0, M);
        assert!(p[0].1.contains("result.md"), "got: {}", p[0].1);
        assert!(p[0].1.contains("reporch task finish"), "got: {}", p[0].1);
    }

    #[test]
    fn a_never_briefed_worker_does_not_block_the_completion_handoff() {
        let s = state();
        let (s, _) = s.step(Event::SessionInitialised {
            repo: M.to_string(),
            advertised_denied_tools: vec![],
            at_ms: 0,
        });
        let (s, _) = s.step(Event::SessionInitialised {
            repo: UI.to_string(),
            advertised_denied_tools: vec![],
            at_ms: 0,
        });
        assert_eq!(
            s.state_of(JS),
            Some(RepoState::Spawning),
            "the uninvolved process has never received a paid first turn"
        );
        let (s, _) = s.step(Event::BriefFiled {
            id: BriefId::new(1),
            from: M.to_string(),
            to: UI.to_string(),
            kind: BriefKind::Request,
            at_ms: 1,
        });
        let (s, _) = s.step(Event::BriefAcked {
            id: BriefId::new(1),
            by: UI.to_string(),
            status: BriefStatus::Done,
            at_ms: 2,
        });
        let (s, _) = s.step(Event::StatusSet {
            repo: UI.to_string(),
            state: RepoState::Done,
            at_ms: 2,
        });

        let (_, actions) = s.step(Event::Tick { at_ms: 31_000 });
        let pings = pings(&actions);
        assert_eq!(
            pings.len(),
            1,
            "uninvolved `{JS}` must not deadlock: {actions:?}"
        );
        assert_eq!(pings[0].0, M);
        assert!(pings[0].1.contains("result.md"));
    }

    /// The complement of the test above: when a worker is being woken, the master is *not* also
    /// told it is waiting. That would be two pings for one situation, and the second is noise.
    #[test]
    fn the_master_is_not_reminded_while_a_worker_is_being_woken() {
        let s = started();
        let (s, _) = s.step(Event::BriefFiled {
            id: BriefId::new(1),
            from: M.to_string(),
            to: UI.to_string(),
            kind: BriefKind::Request,
            at_ms: 0,
        });
        // UI never reads it. A later tick retries the worker nudge.
        let (_, actions) = s.step(Event::Tick { at_ms: 120_000 });
        let p = pings(&actions);
        assert_eq!(p.len(), 1, "exactly one ping: {p:?}");
        assert_eq!(p[0].0, UI, "the worker, not the master");
    }

    // ------------------------------------------------------- §6.3, guardrails

    #[test]
    fn no_more_than_one_ping_per_session_per_interval() {
        let s = started();
        let (s, a1) = s.step(Event::BriefFiled {
            id: BriefId::new(1),
            from: M.to_string(),
            to: UI.to_string(),
            kind: BriefKind::Request,
            at_ms: 0,
        });
        assert_eq!(pings(&a1).len(), 1);
        // A second brief 1s later must not produce a second ping.
        let (s, a2) = s.step(Event::BriefFiled {
            id: BriefId::new(2),
            from: M.to_string(),
            to: UI.to_string(),
            kind: BriefKind::Request,
            at_ms: 1_000,
        });
        assert!(pings(&a2).is_empty(), "inside the 30s window");
        // After the window, a tick retries and it lands.
        let (_, a3) = s.step(Event::Tick { at_ms: 40_000 });
        assert_eq!(pings(&a3).len(), 1, "retried after the window");
    }

    #[test]
    fn identical_consecutive_bodies_are_dropped() {
        let mut s = started();
        let (ns, _) = s.step(Event::BriefFiled {
            id: BriefId::new(1),
            from: M.to_string(),
            to: UI.to_string(),
            kind: BriefKind::Request,
            at_ms: 0,
        });
        s = ns;
        // Ticks well apart, with nothing changing: the body would be identical each time.
        let mut bodies = Vec::new();
        for t in 1..6 {
            let (ns, a) = s.step(Event::Tick { at_ms: t * 60_000 });
            s = ns;
            bodies.extend(pings(&a).into_iter().map(|(_, b)| b.to_string()));
        }
        for w in bodies.windows(2) {
            assert_ne!(w[0], w[1], "identical consecutive bodies must be dropped");
        }
    }

    #[test]
    fn the_task_ceiling_aborts_rather_than_looping() {
        let limits = PingLimits {
            min_interval_ms: 0,
            max_per_session: 1000,
            max_per_task: 3,
            stale_inbound_ms: u64::MAX,
        };
        let mut s = TaskState::new(M, &repos(), limits);
        for (name, _) in repos() {
            let (ns, _) = s.step(Event::SessionInitialised {
                repo: name,
                advertised_denied_tools: vec![],
                at_ms: 0,
            });
            s = ns;
        }
        let mut aborted = false;
        for i in 0..20u32 {
            let (ns, a) = s.step(Event::BriefFiled {
                id: BriefId::new(i + 1),
                from: M.to_string(),
                to: UI.to_string(),
                kind: BriefKind::Request,
                at_ms: (i as u64) * 1000,
            });
            s = ns;
            if a.iter().any(|x| matches!(x, Action::AbortTask { .. })) {
                aborted = true;
                break;
            }
        }
        assert!(aborted, "the global ceiling must abort the task");
        assert!(s.is_aborted());
        assert!(s.pings_delivered() <= 3);
    }

    #[test]
    fn the_session_ceiling_surfaces_instead_of_aborting() {
        let limits = PingLimits {
            min_interval_ms: 0,
            max_per_session: 2,
            max_per_task: 1000,
            stale_inbound_ms: u64::MAX,
        };
        let mut s = TaskState::new(M, &repos(), limits);
        for (name, _) in repos() {
            let (ns, _) = s.step(Event::SessionInitialised {
                repo: name,
                advertised_denied_tools: vec![],
                at_ms: 0,
            });
            s = ns;
        }
        let mut surfaced = false;
        for i in 0..10u32 {
            let (ns, a) = s.step(Event::BriefFiled {
                id: BriefId::new(i + 1),
                from: M.to_string(),
                to: UI.to_string(),
                kind: BriefKind::Request,
                at_ms: (i as u64) * 1000,
            });
            s = ns;
            surfaced |= a.iter().any(|x| matches!(x, Action::Surface { .. }));
        }
        assert!(!s.is_aborted(), "a session ceiling is not a task-wide loop");
        assert!(surfaced, "it must be surfaced, not silently swallowed");
        assert_eq!(s.pings_received(UI), 2);
    }

    // ------------------------------------------------- §4.4, the spawn assertion

    #[test]
    fn a_session_that_kept_sendmessage_aborts_the_task() {
        let s = state();
        let (s, actions) = s.step(Event::SessionInitialised {
            repo: UI.to_string(),
            advertised_denied_tools: vec!["SendMessage".into(), "ListAgents".into()],
            at_ms: 0,
        });
        assert!(
            s.is_aborted(),
            "the protocol cannot be enforced; do not run"
        );
        assert!(actions
            .iter()
            .any(|a| matches!(a, Action::AbortTask { .. })));
        assert!(matches!(
            s.violations().first(),
            Some(Violation::ToolDenyRulesNotApplied { .. })
        ));
    }

    // ------------------------------------------------------------- §10, budget

    #[test]
    fn cumulative_cost_and_usage_are_assigned_not_accumulated() {
        let s = started();
        let (s, _) = s.step(Event::TurnEnded {
            repo: M.to_string(),
            cumulative_cost_usd: 0.384640,
            cumulative_usage: TokenUsage {
                input: 8,
                output: 552,
                cache_read: 161_114,
                cache_creation: 54_667,
            },
            at_ms: 1000,
        });
        let (s, _) = s.step(Event::TurnEnded {
            repo: M.to_string(),
            cumulative_cost_usd: 0.402591,
            cumulative_usage: TokenUsage {
                input: 10,
                output: 559,
                cache_read: 215_781,
                cache_creation: 54_907,
            },
            at_ms: 2000,
        });
        // Summing would give 0.787; the real session total is the latest value.
        assert!(
            (s.spent_usd(M) - 0.402591).abs() < 1e-9,
            "got {}",
            s.spent_usd(M)
        );
        assert_eq!(s.token_usage(M).output, 559);
        assert_eq!(s.token_usage(M).cache_read, 215_781);
    }

    #[test]
    fn budget_death_is_surfaced_with_the_recovery() {
        let s = started();
        let (s, actions) = s.step(Event::SessionDied {
            repo: UI.to_string(),
            reason: DeathReason::BudgetExhausted,
            at_ms: 1,
        });
        assert_eq!(s.state_of(UI), Some(RepoState::Dead));
        let msg = actions
            .iter()
            .find_map(|a| match a {
                Action::Surface { message } => Some(message.clone()),
                _ => None,
            })
            .expect("must surface");
        assert!(msg.contains("--max-budget-usd"));
        assert!(msg.contains("resume"), "the error must teach the recovery");
    }

    #[test]
    fn teardown_is_not_surfaced_as_a_failure() {
        let s = started();
        let (_, actions) = s.step(Event::SessionDied {
            repo: UI.to_string(),
            reason: DeathReason::TornDown,
            at_ms: 1,
        });
        assert!(!actions.iter().any(|a| matches!(a, Action::Surface { .. })));
    }

    // ------------------------------------------------------------- liveness

    #[test]
    fn reading_and_acking_clears_the_outstanding_work() {
        let s = started();
        let (s, _) = s.step(Event::BriefFiled {
            id: BriefId::new(1),
            from: M.to_string(),
            to: UI.to_string(),
            kind: BriefKind::Request,
            at_ms: 0,
        });
        assert_eq!(s.unread_for(UI), 1);
        let (s, _) = s.step(Event::BriefRead {
            id: BriefId::new(1),
            by: UI.to_string(),
            at_ms: 100,
        });
        assert_eq!(s.unread_for(UI), 0);
        assert_eq!(s.outstanding_for(UI), 1, "read is not done");
        let (s, _) = s.step(Event::BriefAcked {
            id: BriefId::new(1),
            by: UI.to_string(),
            status: BriefStatus::Done,
            at_ms: 200,
        });
        assert_eq!(s.outstanding_for(UI), 0);
        assert!(!s.any_outstanding_briefs());
        assert!(!s.is_quiescent_with_work());
    }

    #[test]
    fn a_brief_only_its_recipient_can_read() {
        let s = started();
        let (s, _) = s.step(Event::BriefFiled {
            id: BriefId::new(1),
            from: M.to_string(),
            to: UI.to_string(),
            kind: BriefKind::Request,
            at_ms: 0,
        });
        let (s, _) = s.step(Event::BriefRead {
            id: BriefId::new(1),
            by: JS.to_string(),
            at_ms: 100,
        });
        assert_eq!(
            s.unread_for(UI),
            1,
            "another repo reading it changes nothing"
        );
    }

    /// §6.3 row 4: a worker idle for too long holding a brief it has **read but not acked**.
    ///
    /// Reading it clears the unread count, so the row-1 arrival nudge cannot fire — which is
    /// exactly the situation row 4 exists to cover: the worker acknowledged the work and then
    /// went quiet. Nobody is pinged; the master is told and decides.
    #[test]
    fn a_stale_unacked_brief_is_surfaced_to_the_master_not_pinged_to_anyone() {
        let s = started();
        let (s, _) = s.step(Event::BriefFiled {
            id: BriefId::new(1),
            from: M.to_string(),
            to: UI.to_string(),
            kind: BriefKind::Request,
            at_ms: 0,
        });
        let (s, _) = s.step(Event::BriefRead {
            id: BriefId::new(1),
            by: UI.to_string(),
            at_ms: 1_000,
        });
        assert_eq!(s.unread_for(UI), 0, "read, so no arrival nudge is due");
        assert_eq!(s.outstanding_for(UI), 1, "but not acked");

        let (s, actions) = s.step(Event::Tick { at_ms: 200_000 });
        assert!(
            pings(&actions).is_empty(),
            "row 4 pings nobody: {actions:?}"
        );
        assert!(
            actions.iter().any(|a| matches!(
                a,
                Action::Surface { message } if message.contains(UI) && message.contains("reporch ping")
            )),
            "must surface to the master, with the command that would fix it: {actions:?}"
        );

        // Surfaced once, not on every tick — otherwise the run screen fills with the same line.
        let (_, again) = s.step(Event::Tick { at_ms: 260_000 });
        assert!(
            !again.iter().any(|a| matches!(
                a,
                Action::Surface { message } if message.contains("idle with an unacked brief")
            )),
            "must not repeat every tick: {again:?}"
        );
    }
}
