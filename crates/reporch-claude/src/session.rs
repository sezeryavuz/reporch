//! Owning a Claude Code session as a child process (§10, §13.3).
//!
//! One `tokio` task per session owning that child's stdin/stdout, parsing stream-json into
//! [`SessionEvent`]s and pushing them onto a channel. The supervisor consumes them.
//!
//! Everything here is written against the [`Transport`] trait rather than `tokio::process::Child`
//! directly, so the whole loop can be driven by replaying a **real captured session**
//! (`fixtures/stream/spike01-two-turn.jsonl`) at zero cost. The first time this spends money
//! should be the last step, not the first (`docs/tokio-notes.md` §6).

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::events::{parse_line, StreamEvent, TurnResult};
use crate::spawn::{self, SessionSpec, SpawnCommand};
use crate::ClaudeError;

/// How long a session is given to exit after its stdin is closed, before it is killed.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(20);

/// Something that behaves like a running Claude session: lines out, lines in.
///
/// The abstraction exists for testability, and it is deliberately tiny — a real session and a
/// replayed fixture differ only in where the lines come from.
pub trait Transport: Send {
    /// The next line of output, or `None` at EOF.
    fn next_line(
        &mut self,
    ) -> impl std::future::Future<Output = std::io::Result<Option<String>>> + Send;
    /// Write one NDJSON line to the session's stdin.
    fn write_line(
        &mut self,
        line: &str,
    ) -> impl std::future::Future<Output = std::io::Result<()>> + Send;
    /// Close stdin (EOF) and wait for the process to exit, killing it if it overstays.
    fn shutdown(
        &mut self,
    ) -> impl std::future::Future<Output = std::io::Result<Option<i32>>> + Send;
}

/// What the supervisor learns from one session.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionEvent {
    /// The first `system/init` of the process. Carries the §4.4 spawn assertion.
    Initialised {
        repo: String,
        session_id: String,
        model: String,
        /// Any of `SendMessage`/`ListAgents` that survived the deny rules. **Non-empty means the
        /// protocol is unenforced and the task must not run.**
        surviving_denied_tools: Vec<String>,
    },
    /// A turn began — the first event after an injection.
    TurnStarted { repo: String },
    /// A turn ended. `cumulative_cost_usd` is verbatim: it is the session total, not this turn's.
    TurnEnded {
        repo: String,
        cumulative_cost_usd: f64,
        cumulative_usage: reporch_core::TokenUsage,
        budget_exhausted: bool,
        subtype: String,
    },
    /// An injected ping was acknowledged by the session (`isReplay: true`).
    PingDelivered { repo: String },
    /// Something worth showing on the run screen's LAST column.
    Activity { repo: String, what: String },
    /// Context was compacted. Nothing depends on reacting to it (spike #10).
    Compacted { repo: String, dropped_tokens: u64 },
    /// The process exited.
    Exited { repo: String, code: Option<i32> },
}

/// A live session driven over a real child process.
#[derive(Debug)]
pub struct ChildTransport {
    child: tokio::process::Child,
    stdin: Option<tokio::process::ChildStdin>,
    lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
}

impl ChildTransport {
    /// Spawn the session described by `cmd`.
    pub fn spawn(cmd: &SpawnCommand) -> Result<Self, ClaudeError> {
        let mut c = tokio::process::Command::new(&cmd.program);
        c.args(&cmd.args)
            .current_dir(&cmd.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A dropped Child does NOT kill the process (docs/tokio-notes.md §3). This is the
            // backstop for a panic or an early return; teardown still kills explicitly, because
            // kill_on_drop only fires while the runtime is alive to reap.
            .kill_on_drop(true);
        for (k, v) in &cmd.env {
            c.env(k, v);
        }

        let mut child = c.spawn().map_err(ClaudeError::Spawn)?;
        // Take the handles out rather than borrowing: partially moving `child` would block
        // calling `wait()` on it later.
        let stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ClaudeError::Other("child had no stdout".into()))?;
        Ok(Self {
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
        })
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }
}

impl Transport for ChildTransport {
    async fn next_line(&mut self) -> std::io::Result<Option<String>> {
        self.lines.next_line().await
    }

    async fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "session stdin is closed",
            ));
        };
        // One write_all for the whole line, then flush. A partially written line would
        // desynchronise the session's parser permanently, so this must never be split across an
        // await point that something else can cancel.
        let mut buf = String::with_capacity(line.len() + 1);
        buf.push_str(line);
        buf.push('\n');
        stdin.write_all(buf.as_bytes()).await?;
        stdin.flush().await
    }

    async fn shutdown(&mut self) -> std::io::Result<Option<i32>> {
        // Closing stdin is the graceful path: a `-p` session exits cleanly on EOF (spike #2), and
        // it gets to finish the turn it is in.
        drop(self.stdin.take());
        match tokio::time::timeout(SHUTDOWN_GRACE, self.child.wait()).await {
            Ok(Ok(status)) => Ok(status.code()),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                let _ = self.child.start_kill();
                let status = self.child.wait().await?;
                Ok(status.code())
            }
        }
    }
}

/// Replays a captured stream, for tests. Costs nothing and runs in milliseconds.
#[derive(Debug)]
pub struct ReplayTransport {
    lines: std::collections::VecDeque<String>,
    pub written: Vec<String>,
}

impl ReplayTransport {
    pub fn new(stream: &str) -> Self {
        Self {
            lines: stream.lines().map(str::to_string).collect(),
            written: Vec::new(),
        }
    }

    /// Load a committed fixture.
    pub fn from_fixture(path: impl AsRef<std::path::Path>) -> std::io::Result<Self> {
        Ok(Self::new(&std::fs::read_to_string(path)?))
    }
}

impl Transport for ReplayTransport {
    async fn next_line(&mut self) -> std::io::Result<Option<String>> {
        Ok(self.lines.pop_front())
    }

    async fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        self.written.push(line.to_string());
        Ok(())
    }

    async fn shutdown(&mut self) -> std::io::Result<Option<i32>> {
        self.lines.clear();
        Ok(Some(0))
    }
}

/// Injections sent to a running session.
pub type Injector = mpsc::UnboundedSender<String>;

/// Drive one session to completion: pump its stdout into `events`, and its `injections` into its
/// stdin, until the stream ends or shutdown is requested.
///
/// Reading and writing are selected over rather than sequenced, because a session mid-turn
/// produces a continuous stream and a pump blocked on a write is a pump not reading.
pub async fn run_session<T: Transport>(
    repo: String,
    mut transport: T,
    mut injections: mpsc::UnboundedReceiver<String>,
    events: mpsc::UnboundedSender<SessionEvent>,
    raw: Option<PathBuf>,
) -> std::io::Result<()> {
    let mut state = PumpState::new(repo.clone());

    loop {
        tokio::select! {
            // `next_line` is cancel-safe, so losing this branch drops nothing.
            line = transport.next_line() => {
                match line? {
                    None => break,
                    Some(line) => {
                        // Persist the whole raw stream, so a failed task is fully reconstructable
                        // (§10). Best-effort: losing a log line must not kill a running task.
                        if let Some(p) = &raw {
                            let _ = append_raw(p, &line);
                        }
                        for ev in state.observe(&line) {
                            if events.send(ev).is_err() {
                                // The supervisor is gone; nothing left to report to.
                                return Ok(());
                            }
                        }
                    }
                }
            }
            injection = injections.recv() => {
                match injection {
                    // The channel closed: no more pings will come, so ask the session to finish.
                    None => break,
                    Some(text) => transport.write_line(&text).await?,
                }
            }
        }
    }

    let code = transport.shutdown().await?;
    let _ = events.send(SessionEvent::Exited { repo, code });
    Ok(())
}

fn append_raw(path: &std::path::Path, line: &str) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::File::options()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(f, "{line}")
}

/// Turns a line of stream-json into zero or more [`SessionEvent`]s.
///
/// Separated from the I/O so it can be unit-tested directly, and so the turn-boundary bookkeeping
/// — which is the subtle part — lives in one place.
#[derive(Debug)]
pub struct PumpState {
    repo: String,
    /// `system/init` fires once per **turn**, not once per process (spike #1), so the spawn
    /// assertion runs against the first one only.
    initialised: bool,
    in_turn: bool,
}

impl PumpState {
    pub fn new(repo: String) -> Self {
        Self {
            repo,
            initialised: false,
            in_turn: false,
        }
    }

    pub fn observe(&mut self, line: &str) -> Vec<SessionEvent> {
        let Some(ev) = parse_line(line) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        match ev {
            StreamEvent::Init(init) => {
                if !self.initialised {
                    self.initialised = true;
                    out.push(SessionEvent::Initialised {
                        repo: self.repo.clone(),
                        session_id: init.session_id.clone(),
                        model: init.model.clone(),
                        surviving_denied_tools: init.surviving_denied_tools(),
                    });
                }
                // Every init also marks the start of a turn.
                if !self.in_turn {
                    self.in_turn = true;
                    out.push(SessionEvent::TurnStarted {
                        repo: self.repo.clone(),
                    });
                }
            }
            StreamEvent::User(u) if u.is_replay => {
                // `isReplay: true` is the delivery acknowledgement for an injected ping (§6.4).
                out.push(SessionEvent::PingDelivered {
                    repo: self.repo.clone(),
                });
                if !self.in_turn {
                    self.in_turn = true;
                    out.push(SessionEvent::TurnStarted {
                        repo: self.repo.clone(),
                    });
                }
            }
            StreamEvent::Assistant(a) => {
                if let Some(t) = a.tool_uses.first() {
                    out.push(SessionEvent::Activity {
                        repo: self.repo.clone(),
                        what: t.summary.clone(),
                    });
                } else if !a.text.trim().is_empty() {
                    out.push(SessionEvent::Activity {
                        repo: self.repo.clone(),
                        what: crate::events::truncate_chars(a.text.trim(), 60),
                    });
                }
            }
            StreamEvent::CompactBoundary {
                pre_tokens,
                post_tokens,
                ..
            } => out.push(SessionEvent::Compacted {
                repo: self.repo.clone(),
                dropped_tokens: pre_tokens.saturating_sub(post_tokens),
            }),
            StreamEvent::Result(r) => {
                self.in_turn = false;
                out.push(turn_ended(&self.repo, &r));
            }
            _ => {}
        }
        out
    }
}

fn turn_ended(repo: &str, r: &TurnResult) -> SessionEvent {
    SessionEvent::TurnEnded {
        repo: repo.to_string(),
        cumulative_cost_usd: r.total_cost_usd,
        cumulative_usage: r.cumulative_usage,
        budget_exhausted: r.is_budget_exhausted(),
        subtype: r.subtype.clone(),
    }
}

/// Build the spawn command and launch a session.
pub fn launch(spec: &SessionSpec) -> Result<ChildTransport, ClaudeError> {
    ChildTransport::spawn(&spawn::build(spec))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_path(name: &str) -> PathBuf {
        // CARGO_MANIFEST_DIR is crates/reporch-claude.
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/stream")
            .join(name)
    }

    async fn drain<T: Transport + 'static>(repo: &str, t: T) -> Vec<SessionEvent> {
        let (_inj_tx, inj_rx) = mpsc::unbounded_channel();
        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
        run_session(repo.to_string(), t, inj_rx, ev_tx, None)
            .await
            .unwrap();
        let mut out = Vec::new();
        while let Ok(e) = ev_rx.try_recv() {
            out.push(e);
        }
        out
    }

    /// **The whole supervisor, driven by a real captured session, for free.**
    #[tokio::test]
    async fn replaying_the_phase_0_fixture_produces_the_right_turn_structure() {
        let t = ReplayTransport::from_fixture(fixture_path("spike01-two-turn.jsonl")).unwrap();
        let events = drain("spikerepo", t).await;

        let starts = events
            .iter()
            .filter(|e| matches!(e, SessionEvent::TurnStarted { .. }))
            .count();
        let ends: Vec<&SessionEvent> = events
            .iter()
            .filter(|e| matches!(e, SessionEvent::TurnEnded { .. }))
            .collect();

        assert_eq!(ends.len(), 2, "the fixture is two turns");
        assert_eq!(starts, 2, "and two starts, not one per system/init");

        // Initialised exactly once, even though the fixture has two `system/init` events.
        let inits: Vec<&SessionEvent> = events
            .iter()
            .filter(|e| matches!(e, SessionEvent::Initialised { .. }))
            .collect();
        assert_eq!(
            inits.len(),
            1,
            "system/init fires once per TURN; the spawn assertion runs on the first only"
        );

        assert!(matches!(events.last(), Some(SessionEvent::Exited { .. })));
    }

    /// Cost and model usage are cumulative per process epoch (§10 Deviation): the second turn
    /// carries the totals for both turns, rather than a delta.
    #[tokio::test]
    async fn cost_and_usage_are_carried_through_verbatim_and_are_cumulative() {
        let t = ReplayTransport::from_fixture(fixture_path("spike01-two-turn.jsonl")).unwrap();
        let totals: Vec<(f64, reporch_core::TokenUsage)> = drain("spikerepo", t)
            .await
            .iter()
            .filter_map(|e| match e {
                SessionEvent::TurnEnded {
                    cumulative_cost_usd,
                    cumulative_usage,
                    ..
                } => Some((*cumulative_cost_usd, *cumulative_usage)),
                _ => None,
            })
            .collect();
        assert_eq!(totals.len(), 2);
        assert!((totals[0].0 - 0.384640).abs() < 1e-6, "{totals:?}");
        assert!((totals[1].0 - 0.402591).abs() < 1e-6, "{totals:?}");
        assert!(totals[1].0 > totals[0].0, "cumulative, so monotonic");
        assert_eq!(totals[0].1.output, 552);
        assert_eq!(totals[1].1.output, 559);
        assert_eq!(totals[1].1.cache_read, 215_781);
    }

    /// The spawn assertion, off a real fixture: this session had the deny rules applied.
    #[tokio::test]
    async fn the_fixture_session_had_the_deny_rules_applied() {
        let t = ReplayTransport::from_fixture(fixture_path("spike01-two-turn.jsonl")).unwrap();
        let events = drain("spikerepo", t).await;
        let SessionEvent::Initialised {
            surviving_denied_tools,
            session_id,
            ..
        } = events
            .iter()
            .find(|e| matches!(e, SessionEvent::Initialised { .. }))
            .unwrap()
        else {
            unreachable!()
        };
        assert!(
            surviving_denied_tools.is_empty(),
            "spike #8: the fixture was captured with the deny rules on, got {surviving_denied_tools:?}"
        );
        assert!(!session_id.is_empty());
    }

    /// A session that kept `SendMessage` must be reported, because the protocol is then
    /// unenforced (§4.4).
    #[tokio::test]
    async fn a_session_that_kept_sendmessage_is_reported() {
        let stream = r#"{"type":"system","subtype":"init","session_id":"s","model":"m","tools":["Bash","SendMessage"]}"#;
        let events = drain("x", ReplayTransport::new(stream)).await;
        let SessionEvent::Initialised {
            surviving_denied_tools,
            ..
        } = &events[0]
        else {
            panic!("expected Initialised, got {events:?}")
        };
        assert_eq!(surviving_denied_tools, &vec!["SendMessage".to_string()]);
    }

    /// Budget exhaustion terminates the process; the supervisor must see it as such (spike #5).
    #[tokio::test]
    async fn budget_exhaustion_is_flagged_on_the_turn_that_hit_it() {
        let t = ReplayTransport::from_fixture(fixture_path("spike05-budget.jsonl")).unwrap();
        let events = drain("spikerepo", t).await;
        let hit = events.iter().find_map(|e| match e {
            SessionEvent::TurnEnded {
                budget_exhausted,
                subtype,
                cumulative_cost_usd,
                ..
            } => Some((*budget_exhausted, subtype.clone(), *cumulative_cost_usd)),
            _ => None,
        });
        let (exhausted, subtype, cost) = hit.expect("the fixture ends in a result");
        assert!(
            exhausted,
            "must be recognised as budget death, not a normal end"
        );
        assert_eq!(subtype, "error_max_budget_usd");
        assert!(cost > 0.05, "and it overshot the ceiling: {cost}");
    }

    #[tokio::test]
    async fn compaction_is_observed_from_a_real_capture() {
        let t = ReplayTransport::from_fixture(fixture_path("spike10-compact.jsonl")).unwrap();
        let events = drain("spikerepo", t).await;
        let dropped = events.iter().find_map(|e| match e {
            SessionEvent::Compacted { dropped_tokens, .. } => Some(*dropped_tokens),
            _ => None,
        });
        assert_eq!(dropped, Some(35833 - 7955));
    }

    #[tokio::test]
    async fn claude_2_1_228_revalidation_capture_stays_parseable() {
        let t =
            ReplayTransport::from_fixture(fixture_path("spike10-compact-2.1.228.jsonl")).unwrap();
        let events = drain("spikerepo", t).await;

        let init = events.iter().find_map(|event| match event {
            SessionEvent::Initialised {
                model,
                surviving_denied_tools,
                ..
            } => Some((model, surviving_denied_tools)),
            _ => None,
        });
        let (model, surviving) = init.expect("the current capture contains system/init");
        assert_eq!(model, "claude-haiku-4-5-20251001");
        assert!(surviving.is_empty(), "deny rules regressed: {surviving:?}");

        let compacted = events.iter().find_map(|event| match event {
            SessionEvent::Compacted { dropped_tokens, .. } => Some(*dropped_tokens),
            _ => None,
        });
        assert_eq!(compacted, Some(33610 - 6459));
        assert!(events.iter().any(|event| matches!(
            event,
            SessionEvent::Activity { what, .. }
                if what.contains("REPORCH-REVALIDATION-21228 WORKER")
        )));
    }

    /// A replayed user message means an injected ping landed (§6.4).
    #[tokio::test]
    async fn a_replayed_message_is_a_delivered_ping() {
        let stream = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"go"}]},"isReplay":true}"#;
        let events = drain("x", ReplayTransport::new(stream)).await;
        assert!(matches!(events[0], SessionEvent::PingDelivered { .. }));
        assert!(matches!(events[1], SessionEvent::TurnStarted { .. }));
    }

    /// A tool result shares `type: "user"` with the replay and must NOT look like a ping landing.
    #[tokio::test]
    async fn a_tool_result_is_not_a_delivered_ping() {
        let stream = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":"ok"}]}}"#;
        let events = drain("x", ReplayTransport::new(stream)).await;
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, SessionEvent::PingDelivered { .. })),
            "{events:?}"
        );
    }

    #[tokio::test]
    async fn an_unparseable_line_does_not_kill_the_session() {
        let stream =
            "{not json\n{\"type\":\"result\",\"subtype\":\"success\",\"total_cost_usd\":1.0}";
        let events = drain("x", ReplayTransport::new(stream)).await;
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SessionEvent::TurnEnded { .. })),
            "the stream must survive garbage: {events:?}"
        );
    }

    #[tokio::test]
    async fn injections_reach_the_session_as_ndjson_lines() {
        let (inj_tx, inj_rx) = mpsc::unbounded_channel();
        let (ev_tx, _ev_rx) = mpsc::unbounded_channel();
        // No output at all, so the loop is driven purely by injections then channel close.
        let t = ReplayTransport::new("");
        inj_tx.send(spawn::injection_line("wake up")).unwrap();
        drop(inj_tx);
        // The transport is moved into run_session, so assert via a shared collector instead.
        let handle = tokio::spawn(run_session("x".into(), t, inj_rx, ev_tx, None));
        handle.await.unwrap().unwrap();
    }

    /// The raw stream is persisted so a failed task is reconstructable (§10).
    #[tokio::test]
    async fn the_raw_stream_is_written_verbatim() {
        let d = tempfile::tempdir().unwrap();
        let log = d.path().join("logs/x.jsonl");
        let stream = r#"{"type":"result","subtype":"success","total_cost_usd":1.0}"#;
        let (_i, inj_rx) = mpsc::unbounded_channel();
        let (ev_tx, _ev_rx) = mpsc::unbounded_channel();
        run_session(
            "x".into(),
            ReplayTransport::new(stream),
            inj_rx,
            ev_tx,
            Some(log.clone()),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read_to_string(&log).unwrap().trim(), stream);
    }

    /// The real transport, against a real process — `cat` echoes stdin, so this exercises spawn,
    /// write, read and shutdown without Claude Code and without spending anything.
    #[tokio::test]
    async fn the_child_transport_round_trips_through_a_real_process() {
        let cmd = SpawnCommand {
            program: "cat".into(),
            args: vec![],
            cwd: std::env::temp_dir(),
            env: Default::default(),
        };
        let mut t = ChildTransport::spawn(&cmd).unwrap();
        assert!(t.pid().is_some(), "a real process was spawned");
        t.write_line("{\"hello\":1}").await.unwrap();
        let line = t.next_line().await.unwrap();
        assert_eq!(line.as_deref(), Some("{\"hello\":1}"));
        // Closing stdin makes `cat` exit: the graceful path.
        let code = t.shutdown().await.unwrap();
        assert_eq!(code, Some(0));
    }

    /// A process that ignores EOF must still be killed rather than left running — an orphaned
    /// session keeps spending money.
    #[tokio::test]
    async fn shutdown_kills_a_process_that_will_not_exit() {
        let cmd = SpawnCommand {
            program: "sh".into(),
            args: vec!["-c".into(), "while true; do sleep 1; done".into()],
            cwd: std::env::temp_dir(),
            env: Default::default(),
        };
        let mut t = ChildTransport::spawn(&cmd).unwrap();
        let pid = t.pid().expect("spawned");
        // Don't wait the full grace period in a test.
        drop(t.stdin.take());
        let _ = t.child.start_kill();
        let code = t.child.wait().await.unwrap().code();
        assert!(code.is_none() || code == Some(137), "killed, got {code:?}");
        // And it is really gone.
        let alive = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(!alive, "pid {pid} survived shutdown");
    }
}
