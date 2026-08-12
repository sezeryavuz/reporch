//! The task run loop (§13.3).
//!
//! ```text
//! one tokio task per session  ──SessionEvent──►  supervisor  ──Action──►  effects
//!                                                     │
//!                                                TaskState (owned, never shared)
//! ```
//!
//! The supervisor is the only owner of [`TaskState`]. Nothing else can reach it, which is how
//! reporch avoids ever holding a lock across an `.await` (`docs/tokio-notes.md` §3): the state
//! does not need to be reachable from anywhere else, because the transition is a pure function
//! and everything talks to it over channels.
//!
//! This is also where the two halves of §6 meet. The state machine decides *whether* a ping is
//! warranted; the supervisor is the only thing that can actually deliver one, because it holds
//! every session's stdin.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;

use reporch_claude::SessionEvent;
use reporch_core::brief::{BriefId, PingBy};
use reporch_core::protocol::{Action, DeathReason, Event, PingLimits, TaskState};
use reporch_core::{RepoState, Role, Task, TokenUsage};
use reporch_store::{atomic, mailbox, TaskPaths};
use tokio::sync::mpsc;

/// How often the supervisor evaluates liveness (§6.3). This tick is what makes the anti-deadlock
/// guarantee actually fire.
pub const TICK: Duration = Duration::from_secs(5);

/// Something the run screen needs to show, or the user needs to see.
#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    /// A line for the run screen's message area.
    Surface(String),
    /// A ping was delivered.
    Pinged { to: String, reason: String },
    /// The task ended, one way or another.
    Finished { reason: FinishReason },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinishReason {
    /// The master called `reporch task finish`.
    Completed,
    /// A ping loop, or a session whose deny rules did not apply.
    Aborted(String),
    /// The user asked to stop.
    Cancelled,
    /// The configured wall-clock bound elapsed; worktrees remain available for resumption.
    Delayed(String),
}

/// A live view of the task, for the run screen (§5.8).
#[derive(Debug, Clone, Default)]
pub struct RunView {
    pub rows: Vec<RunRow>,
    pub spent_usd: f64,
    pub budget_usd: f64,
    pub token_usage: TokenUsage,
    pub notices: Vec<String>,
    pub finished: Option<FinishReason>,
    /// Only attention delivery is paused; output, spend and terminal conditions remain live.
    pub paused: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunRow {
    pub repo: String,
    pub role: Role,
    pub state: RepoState,
    /// The LAST column: what the session is doing right now.
    pub last: String,
    pub briefs_in: usize,
    pub briefs_out: usize,
    pub spent_usd: f64,
    pub token_usage: TokenUsage,
}

/// Everything the supervisor needs to drive one task.
pub struct Supervisor {
    task: Task,
    paths: TaskPaths,
    state: TaskState,
    /// One injector per session — the stdin end. This is what makes the control plane able to
    /// ping at all (§6.4).
    injectors: BTreeMap<String, mpsc::UnboundedSender<String>>,
    last: BTreeMap<String, String>,
    notices: Vec<String>,
    /// Offsets already consumed from the CLI's append-only request files, so a re-read does not
    /// replay them.
    consumed_pings: usize,
    consumed_status: usize,
    /// Briefs already reported to the state machine. The mailbox is the source of truth and the
    /// sessions write to it directly, so the supervisor discovers briefs by reading rather than
    /// by being told — and must not report the same one twice.
    seen_briefs: Vec<BriefId>,
    finish_override: Option<FinishReason>,
    paused: bool,
    pending_pings: Vec<PendingPing>,
    /// Transport truth, kept separate from model-reported statuses. A master may set itself
    /// `done` and call `task finish` inside one Bash tool, but the enclosing Claude turn still
    /// has a final result event carrying its cumulative spend and token totals.
    turns_in_progress: BTreeSet<String>,
}

struct PendingPing {
    to: String,
    body: String,
    by: PingBy,
    reason: String,
}

impl Supervisor {
    pub fn new(
        task: Task,
        paths: TaskPaths,
        limits: PingLimits,
        injectors: BTreeMap<String, mpsc::UnboundedSender<String>>,
    ) -> Self {
        let repos: Vec<(String, Role)> = task
            .repositories
            .iter()
            .map(|r| (r.name.clone(), r.role))
            .collect();
        let state = TaskState::new(task.master_repo.clone(), &repos, limits);
        Self {
            task,
            paths,
            state,
            injectors,
            last: BTreeMap::new(),
            notices: Vec::new(),
            consumed_pings: 0,
            consumed_status: 0,
            seen_briefs: Vec::new(),
            finish_override: None,
            paused: false,
            pending_pings: Vec::new(),
            turns_in_progress: BTreeSet::new(),
        }
    }

    pub fn view(&self) -> RunView {
        let idx = mailbox::load_index(&self.paths).unwrap_or_default();
        let rows = self
            .task
            .repositories
            .iter()
            .map(|r| RunRow {
                repo: r.name.clone(),
                role: r.role,
                state: self.state.state_of(&r.name).unwrap_or(RepoState::Idle),
                last: self
                    .last
                    .get(&r.name)
                    .cloned()
                    .unwrap_or_else(|| "─".into()),
                briefs_in: idx.briefs().filter(|b| b.to == r.name).count(),
                briefs_out: idx.briefs().filter(|b| b.from == r.name).count(),
                spent_usd: self.state.spent_usd(&r.name),
                token_usage: self.state.token_usage(&r.name),
            })
            .collect();
        RunView {
            rows,
            spent_usd: self.state.total_spent_usd(),
            budget_usd: self.task.budget_usd,
            token_usage: self.state.total_token_usage(),
            notices: self.notices.clone(),
            finished: if let Some(reason) = &self.finish_override {
                Some(reason.clone())
            } else if self.state.is_completed() {
                Some(FinishReason::Completed)
            } else if self.state.is_aborted() {
                Some(FinishReason::Aborted(
                    self.notices.last().cloned().unwrap_or_default(),
                ))
            } else {
                None
            },
            paused: self.paused,
        }
    }

    /// Translate one session event into a protocol event, step the machine, and perform the
    /// actions it returns.
    pub fn on_session_event(&mut self, ev: SessionEvent, now_ms: u64) {
        // Activity is display-only: it never reaches the protocol, because what a session is
        // *doing* has no bearing on who may ping whom.
        if let SessionEvent::Activity { repo, what } = &ev {
            self.last.insert(repo.clone(), what.clone());
            return;
        }

        match &ev {
            SessionEvent::TurnStarted { repo } => {
                self.turns_in_progress.insert(repo.clone());
            }
            SessionEvent::TurnEnded { repo, .. } | SessionEvent::Exited { repo, .. } => {
                self.turns_in_progress.remove(repo);
            }
            _ => {}
        }

        let protocol_event = match ev {
            SessionEvent::Initialised {
                repo,
                surviving_denied_tools,
                ..
            } => Event::SessionInitialised {
                repo,
                advertised_denied_tools: surviving_denied_tools,
                at_ms: now_ms,
            },
            SessionEvent::TurnStarted { repo } => Event::TurnStarted {
                repo,
                at_ms: now_ms,
            },
            SessionEvent::TurnEnded {
                repo,
                cumulative_cost_usd,
                cumulative_usage,
                budget_exhausted,
                ..
            } => {
                if budget_exhausted {
                    // Hitting the ceiling terminates the process; it does not pause (spike #5).
                    self.apply(Event::TurnEnded {
                        repo: repo.clone(),
                        cumulative_cost_usd,
                        cumulative_usage,
                        at_ms: now_ms,
                    });
                    Event::SessionDied {
                        repo,
                        reason: DeathReason::BudgetExhausted,
                        at_ms: now_ms,
                    }
                } else {
                    Event::TurnEnded {
                        repo,
                        cumulative_cost_usd,
                        cumulative_usage,
                        at_ms: now_ms,
                    }
                }
            }
            SessionEvent::Exited { repo, code } => Event::SessionDied {
                repo,
                reason: if code == Some(0) {
                    DeathReason::TornDown
                } else {
                    DeathReason::Crashed
                },
                at_ms: now_ms,
            },
            SessionEvent::Compacted {
                repo,
                dropped_tokens,
            } => {
                // Nothing depends on reacting to compaction (spike #10 confirmed the protocol
                // survives it), but it belongs on the timeline.
                self.notices.push(format!(
                    "`{repo}` compacted its context, dropping {dropped_tokens} tokens"
                ));
                return;
            }
            SessionEvent::PingDelivered { .. } => return,
            SessionEvent::Activity { .. } => unreachable!("handled above"),
        };
        self.apply(protocol_event);
        // `task finish` writes its marker during the master's turn. Complete immediately after
        // that turn's result has been accounted for rather than waiting for the next 5s tick.
        self.check_finish_marker();
    }

    /// The periodic tick: drain what the CLI queued, then evaluate liveness (§6.3).
    pub fn tick(&mut self, now_ms: u64) {
        self.drain_briefs(now_ms);
        self.drain_ping_requests(now_ms);
        self.drain_status_updates(now_ms);
        self.apply(Event::Tick { at_ms: now_ms });
        self.check_finish_marker();
    }

    /// Notice briefs the sessions filed through the CLI.
    ///
    /// The mailbox is the source of truth and the sessions write to it directly, so the
    /// supervisor learns about a brief by reading the index rather than by being told.
    fn drain_briefs(&mut self, now_ms: u64) {
        let idx = match mailbox::load_index(&self.paths) {
            Ok(i) => i,
            Err(_) => return,
        };
        let known: Vec<BriefId> = idx.briefs().map(|b| b.id).collect();
        for id in known {
            if self.seen_briefs.contains(&id) {
                continue;
            }
            self.seen_briefs.push(id);
            if let Some(b) = idx.brief(id) {
                self.apply(Event::BriefFiled {
                    id,
                    from: b.from.clone(),
                    to: b.to.clone(),
                    kind: b.kind,
                    at_ms: now_ms,
                });
            }
        }
        // Statuses change under us too — a session acking a brief is what clears its inbox.
        for b in idx.briefs() {
            if b.status.is_closed() {
                self.apply(Event::BriefAcked {
                    id: b.id,
                    by: b.to.clone(),
                    status: b.status,
                    at_ms: now_ms,
                });
            } else if b.read_at.is_some() {
                self.apply(Event::BriefRead {
                    id: b.id,
                    by: b.to.clone(),
                    at_ms: now_ms,
                });
            }
        }
    }

    fn drain_ping_requests(&mut self, now_ms: u64) {
        let path = self.paths.dir().join("ping-requests.jsonl");
        let lines = read_new_lines(&path, &mut self.consumed_pings);
        for line in lines {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            let (Some(to), Some(by)) = (v["to"].as_str(), v["by"].as_str()) else {
                continue;
            };
            self.apply(Event::PingRequested {
                by: by.to_string(),
                target: to.to_string(),
                reason: v["reason"].as_str().map(str::to_string),
                at_ms: now_ms,
            });
        }
    }

    fn drain_status_updates(&mut self, now_ms: u64) {
        let path = self.paths.dir().join("status-updates.jsonl");
        let lines = read_new_lines(&path, &mut self.consumed_status);
        for line in lines {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            let (Some(repo), Some(state)) = (v["repo"].as_str(), v["state"].as_str()) else {
                continue;
            };
            let state = match state {
                "working" => RepoState::Working,
                "waiting" => RepoState::Waiting,
                "blocked" => RepoState::Blocked,
                "done" => RepoState::Done,
                _ => continue,
            };
            if let Some(note) = v["note"].as_str() {
                self.last.insert(repo.to_string(), note.to_string());
            }
            self.apply(Event::StatusSet {
                repo: repo.to_string(),
                state,
                at_ms: now_ms,
            });
        }
    }

    fn check_finish_marker(&mut self) {
        let marker = self.paths.dir().join("finish-requested");
        let master = self.task.master_repo.clone();
        if marker.exists()
            && !self.state.is_completed()
            && !self.turns_in_progress.contains(&master)
        {
            self.apply(Event::FinishRequested {
                by: master,
                at_ms: 0,
            });
        }
    }

    /// Step the state machine and perform every action it returns.
    fn apply(&mut self, event: Event) {
        // `step` consumes the state, so it is swapped out and back. The clone is of a small
        // bookkeeping struct, not of the mailbox.
        let state = std::mem::replace(
            &mut self.state,
            TaskState::new(String::new(), &[], PingLimits::default()),
        );
        let (next, actions) = state.step(event);
        self.state = next;
        for action in actions {
            self.perform(action);
        }
    }

    fn perform(&mut self, action: Action) {
        match action {
            Action::DeliverPing {
                to,
                body,
                by,
                reason,
                ..
            } => {
                if self.paused {
                    self.pending_pings.push(PendingPing {
                        to: to.clone(),
                        body,
                        by,
                        reason: reason.clone(),
                    });
                    self.notices.push(format!(
                        "attention paused — queued ping to `{to}` ({reason})"
                    ));
                } else {
                    self.deliver_ping(to, body, by, reason);
                }
            }
            Action::PersistTask => self.persist(),
            Action::RecordViolation(v) => {
                tracing::warn!(violation = %v, "protocol violation");
                self.notices.push(format!("protocol violation: {v}"));
            }
            Action::Surface { message } => self.notices.push(message),
            Action::AbortTask { reason } => {
                tracing::error!(reason = %reason, "task aborted");
                self.notices.push(format!("task aborted: {reason}"));
            }
            Action::CompleteTask => {
                self.task.status = reporch_core::TaskStatus::Completed;
                self.task.finished_at = Some(reporch_store::now_rfc3339());
                for repo in &mut self.task.repositories {
                    repo.changed = super::merge::has_task_changes(
                        std::path::Path::new(&repo.worktree_path),
                        &repo.base_commit,
                    )
                    .ok();
                }
                self.persist();
                self.notices
                    .push("task complete — open it from `completed tasks` to merge".into());
            }
        }
    }

    /// Write `task.json` back, reflecting the live state.
    fn persist(&mut self) {
        for r in &mut self.task.repositories {
            if let Some(s) = self.state.state_of(&r.name) {
                r.state = s;
            }
            r.spent_usd = self.state.spent_usd(&r.name);
            r.token_usage = self.state.token_usage(&r.name);
        }
        self.task.spent_usd = self.state.total_spent_usd();
        if let Err(e) = atomic::write_json(&self.paths.task_json(), &self.task) {
            tracing::warn!(error = %e, "could not persist task.json");
        }
    }

    pub fn is_finished(&self) -> bool {
        self.finish_override.is_some() || self.state.is_completed() || self.state.is_aborted()
    }

    pub fn state(&self) -> &TaskState {
        &self.state
    }

    /// Close every session's injector, which asks each one to finish and exit (§11.3).
    pub fn shutdown(&mut self) {
        self.injectors.clear();
        self.pending_pings.clear();
    }

    /// Suspend or resume attention delivery without hiding output or terminal conditions.
    pub fn set_paused(&mut self, paused: bool) {
        if self.is_finished() || self.paused == paused {
            return;
        }
        self.paused = paused;
        if paused {
            self.notices.push(
                "attention paused — active turns continue, but reporch will inject no pings".into(),
            );
            return;
        }

        self.notices.push("attention resumed".into());
        for pending in std::mem::take(&mut self.pending_pings) {
            self.deliver_ping(pending.to, pending.body, pending.by, pending.reason);
        }
    }

    /// Record a user-requested stop before closing session stdin. The task remains inspectable and
    /// resumable; cancellation never removes worktrees or pretends the task completed (§5.8).
    pub fn cancel(&mut self) {
        if self.is_finished() || self.finish_override.is_some() {
            return;
        }
        self.task.status = reporch_core::TaskStatus::Cancelled;
        self.task.finished_at = Some(reporch_store::now_rfc3339());
        self.finish_override = Some(FinishReason::Cancelled);
        self.notices
            .push("task cancelled by user — worktrees were preserved".into());
        self.persist();
        self.shutdown();
    }

    /// Stop a task at its wall-clock ceiling without misreporting it as failed or completed.
    pub fn delay(&mut self, reason: String) {
        if self.is_finished() || self.finish_override.is_some() {
            return;
        }
        self.task.status = reporch_core::TaskStatus::Delayed;
        self.task.finished_at = Some(reporch_store::now_rfc3339());
        self.finish_override = Some(FinishReason::Delayed(reason.clone()));
        self.notices.push(reason);
        self.persist();
        self.shutdown();
    }

    /// Record a control-plane failure and stop every session without calling it user cancellation.
    pub fn fail(&mut self, reason: String) {
        if self.is_finished() || self.finish_override.is_some() {
            return;
        }
        self.task.status = reporch_core::TaskStatus::Failed;
        self.task.finished_at = Some(reporch_store::now_rfc3339());
        self.finish_override = Some(FinishReason::Aborted(reason.clone()));
        self.notices.push(format!("task failed: {reason}"));
        self.persist();
        self.shutdown();
    }

    fn deliver_ping(&mut self, to: String, body: String, by: PingBy, reason: String) {
        // Delivering a ping is a line written to that process's stdin (§6.4).
        let Some(tx) = self.injectors.get(&to) else {
            self.notices
                .push(format!("could not reach `{to}`: its session is gone"));
            return;
        };
        let line = reporch_claude::spawn::injection_line(&body);
        if tx.send(line).is_err() {
            self.notices
                .push(format!("could not reach `{to}`: its session is gone"));
            return;
        }

        // Every ping is logged with its trigger, so a ping storm is visible after the fact rather
        // than inferred (§6.3). A paused ping is recorded here, on actual delivery.
        let _ = mailbox::record_ping(&self.paths, &to, by, &reason, &reporch_store::now_rfc3339());
        self.notices.push(match by {
            PingBy::Master => format!("`{to}` pinged by the master — {reason}"),
            PingBy::ControlPlane => format!("`{to}` pinged by reporch — {reason}"),
        });
    }
}

/// Read lines appended since the last call, advancing `consumed`.
///
/// The CLI's request files are append-only, so tracking a line offset is enough and there is no
/// need to lock: a partially written last line is simply not seen until the next tick.
fn read_new_lines(path: &PathBuf, consumed: &mut usize) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let all: Vec<&str> = text.lines().collect();
    if all.len() <= *consumed {
        return Vec::new();
    }
    let fresh: Vec<String> = all[*consumed..].iter().map(|s| s.to_string()).collect();
    *consumed = all.len();
    fresh
}

#[cfg(test)]
mod tests {
    use super::*;
    use reporch_core::brief::{AckStatus, BriefKind};
    use reporch_core::{TaskRepo, TaskStatus};

    const M: &str = "modulex";
    const UI: &str = "modulex-ui";

    fn repo(name: &str, role: Role) -> TaskRepo {
        TaskRepo {
            name: name.into(),
            path: format!("/r/{name}"),
            role,
            worktree_path: format!("/wt/{name}"),
            branch: "b".into(),
            base_commit: "a1".into(),
            session_alias: format!("{name}@TASK-0001"),
            session_id: "s".into(),
            session_pid: None,
            model: "opus".into(),
            effort: reporch_core::Effort::Max,
            ultracode: false,
            state: RepoState::Spawning,
            changed: None,
            last_activity_at: None,
            spent_usd: 0.0,
            token_usage: Default::default(),
        }
    }

    fn fixture() -> (
        tempfile::TempDir,
        Supervisor,
        mpsc::UnboundedReceiver<String>,
        mpsc::UnboundedReceiver<String>,
    ) {
        let d = tempfile::tempdir().unwrap();
        let paths = TaskPaths::at(d.path().join("t"));
        std::fs::create_dir_all(paths.briefs_dir()).unwrap();
        let task = Task {
            schema_version: 1,
            id: "TASK-0001".parse().unwrap(),
            name: "x".into(),
            status: TaskStatus::Active,
            created_at: "2026-08-07T21:40:00Z".into(),
            started_at: None,
            finished_at: None,
            master_repo: M.into(),
            model: "opus".into(),
            master_effort: reporch_core::Effort::Max,
            worker_effort: reporch_core::Effort::XHigh,
            ultracode: false,
            budget_usd: 10.0,
            spent_usd: 0.0,
            branch: "b".into(),
            repositories: vec![repo(M, Role::Master), repo(UI, Role::Worker)],
        };
        let (ui_tx, ui_rx) = mpsc::unbounded_channel();
        let (m_tx, m_rx) = mpsc::unbounded_channel();
        let mut inj = BTreeMap::new();
        inj.insert(UI.to_string(), ui_tx);
        inj.insert(M.to_string(), m_tx);
        let sup = Supervisor::new(task, paths, PingLimits::default(), inj);
        (d, sup, ui_rx, m_rx)
    }

    fn bring_up(sup: &mut Supervisor) {
        for r in [M, UI] {
            sup.on_session_event(
                SessionEvent::Initialised {
                    repo: r.into(),
                    session_id: "s".into(),
                    model: "m".into(),
                    surviving_denied_tools: vec![],
                },
                0,
            );
        }
    }

    /// **The end-to-end path the whole product exists for**: a brief lands for an idle worker,
    /// and reporch delivers a ping down that session's stdin.
    #[test]
    fn a_brief_for_an_idle_worker_becomes_a_line_on_its_stdin() {
        let (_d, mut sup, mut ui_rx, _m_rx) = fixture();
        bring_up(&mut sup);

        mailbox::send(
            &sup.paths,
            "TASK-0001",
            M,
            UI,
            BriefKind::Request,
            "Add the settings screen",
            "body",
            None,
            "2026-08-07T21:44:12Z",
        )
        .unwrap();

        sup.tick(1000);

        let line = ui_rx.try_recv().expect("a ping must have been delivered");
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["type"], "user", "a ping is an injected user turn (§6.4)");
        let text = v["message"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("reporch brief read 0001"), "got: {text}");

        // And it was logged with its trigger (§6.3).
        let idx = mailbox::load_index(&sup.paths).unwrap();
        assert_eq!(idx.pings().count(), 1);
        assert_eq!(idx.pings().next().unwrap().to, UI);
    }

    #[test]
    fn the_last_worker_ack_becomes_a_completion_turn_on_the_masters_stdin() {
        let (_d, mut sup, mut ui_rx, mut master_rx) = fixture();
        bring_up(&mut sup);

        let filed = mailbox::send(
            &sup.paths,
            "TASK-0001",
            M,
            UI,
            BriefKind::Request,
            "Add the settings screen",
            "body",
            None,
            "2026-08-07T21:44:12Z",
        )
        .unwrap();
        sup.tick(1_000);
        ui_rx.try_recv().expect("the worker receives its brief");
        assert!(
            master_rx.try_recv().is_err(),
            "dispatch is not yet a stalled outbound brief"
        );

        mailbox::read(&sup.paths, filed.id, UI, "2026-08-07T21:45:00Z").unwrap();
        mailbox::ack(
            &sup.paths,
            filed.id,
            UI,
            AckStatus::Done,
            Some("screen shipped"),
            "2026-08-07T21:46:00Z",
        )
        .unwrap();
        atomic::append_line(
            &sup.paths.dir().join("status-updates.jsonl"),
            &serde_json::json!({"repo": UI, "state": "done", "note": "screen shipped"}).to_string(),
        )
        .unwrap();

        sup.tick(31_000);

        let line = master_rx
            .try_recv()
            .expect("the completion handoff must reach the real master injector");
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        let text = value["message"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("result.md"), "got: {text}");
        assert!(text.contains("reporch task finish"), "got: {text}");
        let idx = mailbox::load_index(&sup.paths).unwrap();
        assert!(
            idx.pings().any(|ping| ping.to == M),
            "the completion ping remains auditable"
        );
    }

    #[test]
    fn cancellation_is_persisted_before_session_channels_close() {
        let (_d, mut sup, _ui_rx, _master_rx) = fixture();
        bring_up(&mut sup);

        sup.cancel();

        assert_eq!(sup.view().finished, Some(FinishReason::Cancelled));
        let task: Task = atomic::read_json(&sup.paths.task_json()).unwrap();
        assert_eq!(task.status, TaskStatus::Cancelled);
        assert!(task.finished_at.is_some());
        assert!(sup.injectors.is_empty());
    }

    #[test]
    fn a_worker_ping_request_is_refused_by_the_supervisor_too() {
        let (_d, mut sup, mut ui_rx, _m_rx) = fixture();
        bring_up(&mut sup);
        // The CLI refuses this first, but enforcement must not depend on one layer (§6.2).
        let path = sup.paths.dir().join("ping-requests.jsonl");
        atomic::append_line(
            &path,
            &serde_json::json!({"to": UI, "by": UI, "reason": "please", "at": "x"}).to_string(),
        )
        .unwrap();

        sup.tick(1000);
        assert!(
            ui_rx.try_recv().is_err(),
            "a worker's request must deliver nothing"
        );
        assert!(!sup.state().violations().is_empty());
    }

    #[test]
    fn the_master_ping_request_is_delivered() {
        let (_d, mut sup, mut ui_rx, _m_rx) = fixture();
        bring_up(&mut sup);
        atomic::append_line(
            &sup.paths.dir().join("ping-requests.jsonl"),
            &serde_json::json!({"to": UI, "by": M, "reason": "brief 0007", "at": "x"}).to_string(),
        )
        .unwrap();
        sup.tick(1000);
        assert!(ui_rx.try_recv().is_ok());
    }

    #[test]
    fn pause_queues_attention_but_resume_delivers_it_once() {
        let (_d, mut sup, mut ui_rx, _m_rx) = fixture();
        bring_up(&mut sup);
        sup.set_paused(true);
        atomic::append_line(
            &sup.paths.dir().join("ping-requests.jsonl"),
            &serde_json::json!({"to": UI, "by": M, "reason": "brief 0007", "at": "x"}).to_string(),
        )
        .unwrap();

        sup.tick(1_000);

        assert!(sup.view().paused);
        assert!(ui_rx.try_recv().is_err(), "pause must inject nothing");
        assert_eq!(
            mailbox::load_index(&sup.paths).unwrap().pings().count(),
            0,
            "a queued ping is logged when it is actually delivered"
        );

        sup.set_paused(false);

        assert!(!sup.view().paused);
        assert!(ui_rx.try_recv().is_ok(), "resume delivers the queued ping");
        assert_eq!(
            mailbox::load_index(&sup.paths)
                .unwrap()
                .pings()
                .filter(|ping| ping.to == UI && ping.reason == "brief 0007")
                .count(),
            1
        );
        assert!(ui_rx.try_recv().is_err(), "the ping is delivered once");
    }

    /// A request file is append-only; a tick must not replay what it already consumed.
    #[test]
    fn queued_requests_are_consumed_exactly_once() {
        let (_d, mut sup, mut ui_rx, _m_rx) = fixture();
        bring_up(&mut sup);
        atomic::append_line(
            &sup.paths.dir().join("ping-requests.jsonl"),
            &serde_json::json!({"to": UI, "by": M, "reason": "r", "at": "x"}).to_string(),
        )
        .unwrap();
        sup.tick(1000);
        assert!(ui_rx.try_recv().is_ok());
        sup.tick(200_000);
        assert!(
            ui_rx.try_recv().is_err(),
            "the same request must not fire twice"
        );
    }

    #[test]
    fn a_session_that_kept_sendmessage_aborts_the_task() {
        let (_d, mut sup, _rx, _m_rx) = fixture();
        sup.on_session_event(
            SessionEvent::Initialised {
                repo: UI.into(),
                session_id: "s".into(),
                model: "m".into(),
                surviving_denied_tools: vec!["SendMessage".into()],
            },
            0,
        );
        assert!(sup.is_finished());
        assert!(sup.view().notices.iter().any(|n| n.contains("aborted")));
    }

    #[test]
    fn budget_exhaustion_marks_the_session_dead_and_surfaces_the_recovery() {
        let (_d, mut sup, _rx, _m_rx) = fixture();
        bring_up(&mut sup);
        sup.on_session_event(
            SessionEvent::TurnEnded {
                repo: UI.into(),
                cumulative_cost_usd: 10.5,
                cumulative_usage: Default::default(),
                budget_exhausted: true,
                subtype: "error_max_budget_usd".into(),
            },
            1000,
        );
        let v = sup.view();
        let row = v.rows.iter().find(|r| r.repo == UI).unwrap();
        assert_eq!(row.state, RepoState::Dead);
        assert!(
            (row.spent_usd - 10.5).abs() < 1e-9,
            "cost is still recorded"
        );
        assert!(v.notices.iter().any(|n| n.contains("--max-budget-usd")));
    }

    #[test]
    fn status_updates_from_the_cli_reach_the_run_view() {
        let (_d, mut sup, _rx, _m_rx) = fixture();
        bring_up(&mut sup);
        atomic::append_line(
            &sup.paths.dir().join("status-updates.jsonl"),
            &serde_json::json!({"repo": UI, "state": "done", "note": "screen shipped", "at": "x"})
                .to_string(),
        )
        .unwrap();
        sup.tick(1000);
        let v = sup.view();
        let row = v.rows.iter().find(|r| r.repo == UI).unwrap();
        assert_eq!(row.state, RepoState::Done);
        assert_eq!(row.last, "screen shipped");
    }

    #[test]
    fn activity_reaches_the_run_screen_without_touching_the_protocol() {
        let (_d, mut sup, _rx, _m_rx) = fixture();
        bring_up(&mut sup);
        let before = sup.state().pings_delivered();
        sup.on_session_event(
            SessionEvent::Activity {
                repo: UI.into(),
                what: "editing app/auth/keys.py".into(),
            },
            1000,
        );
        let v = sup.view();
        assert_eq!(
            v.rows.iter().find(|r| r.repo == UI).unwrap().last,
            "editing app/auth/keys.py"
        );
        assert_eq!(sup.state().pings_delivered(), before, "display only");
    }

    #[test]
    fn the_view_totals_spend_across_sessions() {
        let (_d, mut sup, _rx, _m_rx) = fixture();
        bring_up(&mut sup);
        for (repo, cost) in [(M, 0.40), (UI, 0.25)] {
            sup.on_session_event(
                SessionEvent::TurnEnded {
                    repo: repo.into(),
                    cumulative_cost_usd: cost,
                    cumulative_usage: TokenUsage {
                        input: 10,
                        output: 20,
                        cache_read: 30,
                        cache_creation: 40,
                    },
                    budget_exhausted: false,
                    subtype: "success".into(),
                },
                1000,
            );
        }
        let v = sup.view();
        assert!((v.spent_usd - 0.65).abs() < 1e-9, "got {}", v.spent_usd);
        assert_eq!(v.budget_usd, 10.0);
        assert_eq!(v.token_usage.output, 40);
    }

    #[test]
    fn task_json_is_persisted_as_the_run_progresses() {
        let (_d, mut sup, _rx, _m_rx) = fixture();
        bring_up(&mut sup);
        sup.on_session_event(
            SessionEvent::TurnEnded {
                repo: M.into(),
                cumulative_cost_usd: 1.25,
                cumulative_usage: TokenUsage {
                    input: 2,
                    output: 3,
                    cache_read: 5,
                    cache_creation: 7,
                },
                budget_exhausted: false,
                subtype: "success".into(),
            },
            1000,
        );
        let on_disk: Task = atomic::read_json(&sup.paths.task_json()).unwrap();
        assert!((on_disk.spent_usd - 1.25).abs() < 1e-9);
        assert_eq!(on_disk.repo(M).unwrap().token_usage.output, 3);
        assert_eq!(on_disk.repo(M).unwrap().state, RepoState::Idle);
    }

    #[test]
    fn completion_waits_for_the_masters_result_telemetry() {
        let (_d, mut sup, _ui_rx, _master_rx) = fixture();
        bring_up(&mut sup);
        sup.on_session_event(SessionEvent::TurnStarted { repo: M.into() }, 100);
        atomic::write_string(&sup.paths.dir().join("finish-requested"), "now").unwrap();

        sup.tick(200);

        assert!(
            !sup.is_finished(),
            "the finish command runs inside the turn whose result carries final cost and tokens"
        );
        let active: Task = atomic::read_json(&sup.paths.task_json()).unwrap();
        assert_eq!(active.status, TaskStatus::Active);

        sup.on_session_event(
            SessionEvent::TurnEnded {
                repo: M.into(),
                cumulative_cost_usd: 0.42,
                cumulative_usage: TokenUsage {
                    input: 2,
                    output: 3,
                    cache_read: 5,
                    cache_creation: 7,
                },
                budget_exhausted: false,
                subtype: "success".into(),
            },
            300,
        );

        assert!(sup.is_finished());
        let complete: Task = atomic::read_json(&sup.paths.task_json()).unwrap();
        assert_eq!(complete.status, TaskStatus::Completed);
        assert_eq!(complete.repo(M).unwrap().state, RepoState::Idle);
        assert!((complete.repo(M).unwrap().spent_usd - 0.42).abs() < f64::EPSILON);
        assert_eq!(complete.repo(M).unwrap().token_usage.output, 3);
        assert!((complete.spent_usd - 0.42).abs() < f64::EPSILON);
    }

    /// The liveness guarantee, end to end: quiescent with unread briefs must never stay silent.
    #[test]
    fn a_stalled_task_is_never_silent() {
        let (_d, mut sup, mut ui_rx, _m_rx) = fixture();
        bring_up(&mut sup);
        mailbox::send(
            &sup.paths,
            "TASK-0001",
            M,
            UI,
            BriefKind::Request,
            "s",
            "b",
            None,
            "2026-08-07T21:44:12Z",
        )
        .unwrap();

        let mut spoke = false;
        for i in 1..=6u64 {
            sup.tick(i * 60_000);
            if ui_rx.try_recv().is_ok() || !sup.view().notices.is_empty() {
                spoke = true;
                break;
            }
        }
        assert!(
            spoke,
            "the task went quiet with work outstanding and reporch said nothing"
        );
    }
}
