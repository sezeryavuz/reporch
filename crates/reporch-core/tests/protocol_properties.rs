//! Property tests for the orchestration protocol (§6).
//!
//! The unit tests in `protocol.rs` assert specific scenarios. These assert *invariants* — things
//! that must hold for **every** event sequence, including adversarial ones. Four of them are
//! named in the build brief, and the last is the reason this design exists at all.

use proptest::prelude::*;

use reporch_core::brief::{BriefId, BriefKind, BriefStatus, PingBy};
use reporch_core::model::{RepoState, Role, TokenUsage};
use reporch_core::protocol::{Action, DeathReason, Event, PingLimits, TaskState};

const MASTER: &str = "modulex";
const WORKERS: [&str; 3] = ["modulex-ui", "modulex-js", "modulex-docs"];

fn all_repos() -> Vec<(String, Role)> {
    let mut v = vec![(MASTER.to_string(), Role::Master)];
    v.extend(WORKERS.iter().map(|w| (w.to_string(), Role::Worker)));
    v
}

fn names() -> Vec<String> {
    all_repos().into_iter().map(|(n, _)| n).collect()
}

/// A task with every session up and idle.
fn started(limits: PingLimits) -> TaskState {
    let mut s = TaskState::new(MASTER, &all_repos(), limits);
    for (name, _) in all_repos() {
        let (ns, _) = s.step(Event::SessionInitialised {
            repo: name,
            advertised_denied_tools: vec![],
            at_ms: 0,
        });
        s = ns;
    }
    s
}

fn pings(actions: &[Action]) -> Vec<&Action> {
    actions
        .iter()
        .filter(|a| matches!(a, Action::DeliverPing { .. }))
        .collect()
}

/// An arbitrary event, drawn from the whole surface — including illegal ones. A protocol that is
/// only correct on well-behaved input is not enforced, it is merely documented.
fn any_event() -> impl Strategy<Value = Event> {
    let repo = prop::sample::select(names());
    let repo2 = prop::sample::select(names());
    let kind = prop::sample::select(BriefKind::ALL.to_vec());
    let brief_id = (1u32..8).prop_map(BriefId::new);
    let at = 0u64..600_000;

    prop_oneof![
        // Brief traffic, from anyone to anyone — workers writing to workers included (§6.1).
        (
            brief_id.clone(),
            repo.clone(),
            repo2.clone(),
            kind,
            at.clone()
        )
            .prop_map(|(id, from, to, kind, at_ms)| Event::BriefFiled {
                id,
                from,
                to,
                kind,
                at_ms
            }),
        (brief_id.clone(), repo.clone(), at.clone()).prop_map(|(id, by, at_ms)| Event::BriefRead {
            id,
            by,
            at_ms
        }),
        (
            brief_id,
            repo.clone(),
            prop::sample::select(vec![
                BriefStatus::Done,
                BriefStatus::Blocked,
                BriefStatus::Rejected
            ]),
            at.clone()
        )
            .prop_map(|(id, by, status, at_ms)| Event::BriefAcked {
                id,
                by,
                status,
                at_ms
            }),
        // Ping attempts from ANY repo, including workers. This is the adversary.
        (repo.clone(), repo2.clone(), at.clone()).prop_map(|(by, target, at_ms)| {
            Event::PingRequested {
                by,
                target,
                reason: None,
                at_ms,
            }
        }),
        // Session lifecycle.
        (repo.clone(), at.clone()).prop_map(|(repo, at_ms)| Event::TurnStarted { repo, at_ms }),
        (repo.clone(), 0.0f64..5.0, at.clone()).prop_map(|(repo, c, at_ms)| Event::TurnEnded {
            repo,
            cumulative_cost_usd: c,
            cumulative_usage: TokenUsage::default(),
            at_ms
        }),
        (
            repo.clone(),
            prop::sample::select(vec![
                RepoState::Working,
                RepoState::Waiting,
                RepoState::Blocked,
                RepoState::Done,
                RepoState::Idle
            ]),
            at.clone()
        )
            .prop_map(|(repo, state, at_ms)| Event::StatusSet { repo, state, at_ms }),
        (repo.clone(), at.clone()).prop_map(|(repo, at_ms)| Event::SessionDied {
            repo,
            reason: DeathReason::Crashed,
            at_ms
        }),
        at.clone().prop_map(|at_ms| Event::Tick { at_ms }),
    ]
}

/// Events in non-decreasing time order, as a real supervisor would deliver them.
fn event_log(max: usize) -> impl Strategy<Value = Vec<Event>> {
    prop::collection::vec(any_event(), 0..max).prop_map(|mut evs| {
        evs.sort_by_key(|e| e.at_ms());
        evs
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 400, ..ProptestConfig::default() })]

    /// **Property 1 — a worker can never produce a ping.**
    ///
    /// §6.1: workers ping nobody. Not the master, not each other. Every ping in the resulting
    /// action stream must be attributable to the master or to the control plane, and a ping that
    /// followed a worker's `PingRequested` must never exist.
    #[test]
    fn a_worker_can_never_produce_a_ping(log in event_log(60)) {
        let mut state = started(PingLimits::default());
        for ev in log {
            // Is this event a worker asking for a ping?
            let worker_asked = matches!(
                &ev,
                Event::PingRequested { by, .. } if by != MASTER
            );
            let (next, actions) = state.step(ev);
            state = next;
            if worker_asked {
                prop_assert!(
                    pings(&actions).is_empty(),
                    "a worker's ping request produced a ping: {actions:?}"
                );
            }
            // And no ping is ever attributed to a worker.
            for a in &actions {
                if let Action::DeliverPing { by, to, .. } = a {
                    prop_assert!(
                        matches!(by, PingBy::Master | PingBy::ControlPlane),
                        "ping to {to} attributed to neither master nor control plane"
                    );
                }
            }
        }
    }

    /// **Property 2 — a brief landing for an idle master produces exactly one ping.**
    ///
    /// This is the deadlock fix of §6.3, row 2: a worker files a brief for the master and stops.
    /// If the master is quiet, exactly one nudge must follow — not zero (deadlock), not several
    /// (a storm).
    #[test]
    fn a_brief_for_an_idle_master_produces_exactly_one_ping(
        id in 1u32..50,
        from_idx in 0usize..WORKERS.len(),
        kind in prop::sample::select(BriefKind::ALL.to_vec()),
        at_ms in 0u64..1_000_000,
    ) {
        let state = started(PingLimits::default());
        prop_assume!(state.state_of(MASTER).is_some_and(|s| s.is_pingable()));

        let (_, actions) = state.step(Event::BriefFiled {
            id: BriefId::new(id),
            from: WORKERS[from_idx].to_string(),
            to: MASTER.to_string(),
            kind,
            at_ms,
        });
        let p = pings(&actions);
        prop_assert_eq!(p.len(), 1, "expected exactly one ping, got {:?}", p);
        match p[0] {
            Action::DeliverPing { to, body, .. } => {
                prop_assert_eq!(to, MASTER);
                prop_assert!(body.contains("reporch brief inbox"),
                    "the ping must teach the command: {}", body);
            }
            _ => unreachable!(),
        }
    }

    /// **Property 3 — the rate limiter bounds pings under an adversarial event stream.**
    ///
    /// No sequence of events, however hostile, may exceed the per-session or per-task ceilings of
    /// §6.3. This is what makes "runaway loops end in dollars" a bounded statement.
    #[test]
    fn the_rate_limiter_bounds_pings(log in event_log(300)) {
        let limits = PingLimits {
            min_interval_ms: 30_000,
            max_per_session: 5,
            max_per_task: 12,
            stale_inbound_ms: 180_000,
        };
        let mut state = started(limits);
        let mut delivered = 0u32;
        let mut per_target: std::collections::BTreeMap<String, u32> = Default::default();
        let mut last_at: std::collections::BTreeMap<String, u64> = Default::default();

        for ev in log {
            let (next, actions) = state.step(ev);
            state = next;
            for a in &actions {
                if let Action::DeliverPing { to, at_ms, .. } = a {
                    delivered += 1;
                    *per_target.entry(to.clone()).or_default() += 1;
                    // Minimum spacing per session is never violated.
                    if let Some(prev) = last_at.get(to) {
                        prop_assert!(
                            at_ms.saturating_sub(*prev) >= limits.min_interval_ms,
                            "two pings to {to} only {}ms apart", at_ms - prev
                        );
                    }
                    last_at.insert(to.clone(), *at_ms);
                }
            }
        }
        prop_assert!(delivered <= limits.max_per_task,
            "task ceiling breached: {delivered} > {}", limits.max_per_task);
        for (target, n) in &per_target {
            prop_assert!(*n <= limits.max_per_session,
                "session ceiling breached for {target}: {n} > {}", limits.max_per_session);
        }
    }

    /// **Property 4 — no event sequence deadlocks the task.**
    ///
    /// *"That last property is why this design exists. Test it hardest."*
    ///
    /// The failure being excluded: every session quiet, briefs outstanding, and reporch silent —
    /// nine sessions waiting politely and nobody working.
    ///
    /// The guarantee is not "a ping is always sent" — rate limits legitimately delay attention,
    /// and a genuinely stuck task should stop rather than ping forever. The guarantee is that
    /// quiescence-with-work is never *silent*: driving ticks forward must always produce a ping,
    /// a surfaced message, an abort, or a completion.
    #[test]
    fn no_event_sequence_deadlocks(log in event_log(120)) {
        let mut state = started(PingLimits::default());
        let mut now = 0u64;
        for ev in log {
            now = now.max(ev.at_ms());
            let (next, _) = state.step(ev);
            state = next;
            if state.is_aborted() || state.is_completed() {
                return Ok(());
            }
        }

        if !state.is_quiescent_with_work() {
            return Ok(()); // nothing outstanding, or someone is still working: not a deadlock
        }

        // The task is quiet with work outstanding. Drive the clock forward the way the supervisor
        // does. Within a bounded number of ticks, reporch must say *something*.
        let mut spoke = false;
        for i in 1..=12u64 {
            now += 60_000; // comfortably past min_interval_ms
            let (next, actions) = state.step(Event::Tick { at_ms: now });
            state = next;
            if !actions.iter().all(|a| matches!(a, Action::PersistTask)) {
                spoke = true;
                break;
            }
            if state.is_aborted() || state.is_completed() {
                spoke = true;
                break;
            }
            let _ = i;
        }
        prop_assert!(
            spoke,
            "DEADLOCK: quiescent with outstanding briefs and reporch never spoke. \
             quiet={} outstanding={} pings={}",
            state.all_quiet(),
            state.any_outstanding_briefs(),
            state.pings_delivered()
        );
    }

    /// A task that has been aborted or completed stops acting. Otherwise an abort could be
    /// followed by more pings, and "abort" would not mean anything.
    #[test]
    fn a_finished_task_emits_nothing_further(log in event_log(40)) {
        let mut state = started(PingLimits::default());
        let (s, _) = state.step(Event::FinishRequested {
            by: MASTER.to_string(),
            at_ms: 0,
        });
        state = s;
        prop_assert!(state.is_completed());
        for ev in log {
            let (next, actions) = state.step(ev);
            state = next;
            prop_assert!(actions.is_empty(), "completed task still acted: {actions:?}");
        }
    }

    /// Spend is read, never accumulated: `total_cost_usd` is already cumulative per process
    /// (§10 Deviation). Accumulating it would over-report and trip budget ceilings early.
    #[test]
    fn spend_tracks_the_latest_cumulative_value(costs in prop::collection::vec(0.0f64..10.0, 1..20)) {
        let mut state = started(PingLimits::default());
        let mut last = 0.0;
        for (i, c) in costs.iter().enumerate() {
            let (s, _) = state.step(Event::TurnEnded {
                repo: MASTER.to_string(),
                cumulative_cost_usd: *c,
                cumulative_usage: TokenUsage::default(),
                at_ms: i as u64 * 1000,
            });
            state = s;
            last = *c;
        }
        prop_assert!((state.spent_usd(MASTER) - last).abs() < 1e-9);
    }
}
