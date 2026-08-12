# Architecture

reporch has two jobs that must coexist without weakening each other: keep a responsive terminal
application, and own a set of paid child processes until every one has stopped. The design puts
pure protocol decisions at the center and keeps terminal, filesystem, Git and Claude Code effects
at explicit edges.

This document describes the implementation. The normative product rules remain in
[`enhanced_brief.md`](../enhanced_brief.md), especially §6, §8, §10 and §13.

## System map

```text
                                  human keys
                                      │
                                      ▼
┌──────────────┐    effects    ┌───────────────┐    commands    ┌────────────────┐
│ ratatui view │◄──────────────│ console loop  │───────────────►│ task actor     │
│ pure drawing │   snapshots   │ Tokio-aware   │◄───────────────│ one per run    │
└──────────────┘               └───────┬───────┘   RunView      └───────┬────────┘
                                      │                                 │
                         blocking jobs│                    SessionEvent │ stdin wake
                                      ▼                                 ▼
                             ┌────────────────┐                ┌──────────────────┐
                             │ analysis / Git │                │ session pumps    │
                             │ update / merge │                │ Claude processes │
                             └────────────────┘                └────────┬─────────┘
                                                                       │ JSONL
                                          ┌────────────────────────────┘
                                          ▼
                                  ┌─────────────────┐
                                  │ supervisor      │
                                  │ effects + store │
                                  └────────┬────────┘
                                           │ Event
                                           ▼
                                  ┌─────────────────┐
                                  │ reporch-core    │
                                  │ pure TaskState  │
                                  │ → Vec<Action>   │
                                  └─────────────────┘
```

The arrows matter more than the boxes. The UI never owns a Claude child, a session pump never
mutates protocol state, and the pure state machine never performs an effect.

## Workspace boundaries

The workspace has six publishable crates plus a non-publishable release helper.

| Crate | Owns | Must not own |
| :--- | :--- | :--- |
| `reporch-core` | IDs, persisted domain shapes, dependency graph, effort ladder, brief states and the protocol transition | Files, clocks, processes, networking, terminal rendering |
| `reporch-store` | `$REPORCH_HOME`, atomic JSON/Markdown writes, advisory locks, config and mailbox files | Git or Claude process behavior |
| `reporch-claude` | Version checks, spawn command construction, process transport, stream-json parsing, registry/orphan reads | Task policy or terminal state |
| `reporch-git` | Real `git` commands, worktrees, carry-over, hooks, branch and merge operations | Reimplementing Git semantics in a library |
| `reporch-scan` | Parallel repository discovery, cache inputs, fuzzy and relationship ranking | Project analysis or task orchestration |
| `reporch` | CLI, console, project analysis, task actor, supervisor, merge review and integration of the five libraries | A second implementation of their rules |
| `xtask` | Local verification and non-publishing package checks | Runtime behavior or a publish command |

All crates forbid unsafe Rust. The particularly important boundary is `reporch-core`: its
`tests/no_io.rs` walks Cargo's resolved normal-dependency graph and fails if anything outside a
small pure allow-list appears. Adding Tokio, tracing, a clock, a filesystem crate or a terminal
crate is therefore a test failure, not merely a review convention.

## The console loop

The binary creates one multi-thread Tokio runtime, then runs the console on it. Each frame follows
the same sequence:

1. Draw the current `App` state.
2. Drain crossterm with a zero-duration poll. No terminal read blocks Tokio.
3. Turn a key into a small `Effect` value.
4. Start the effect on the appropriate owner: an async task, Tokio's blocking pool, or the running
   task actor.
5. Await the next timer tick or channel event with `tokio::select!`.

Repository scanning and Git/analysis preparation are blocking work and never execute on the async
scheduler. Update checks, browser opening and merge operations also run in background jobs. The UI
continues to redraw progress while they work.

The console allows one active task actor. On exit it aborts background jobs, tells the actor to
cancel, waits up to the bounded child-shutdown window, and aborts the actor only as a last resort.

## From task request to owned processes

`task::runtime::run_task` is the lifecycle owner:

```text
CreateRequest
    │
    ├─ spawn_blocking: allocate ID, create task tree and real Git worktrees
    │
    ├─ spawn every configured Claude child and assign its pre-generated session ID
    │
    ├─ one session pump per child in an owned JoinSet
    │      ├─ write injected NDJSON turns to stdin
    │      ├─ parse stdout into SessionEvent values
    │      └─ append the raw stream to logs/<repo>.jsonl
    │
    ├─ inject the kickoff turn into the elected master
    │
    └─ select! session events, supervisor ticks and human commands
```

The actor owns the `Supervisor`, every stdin injector and the `JoinSet` containing every session
pump. Nothing else can lose a child handle. `ChildTransport` uses kill-on-drop as a cancellation
backstop; ordinary completion first closes injectors and gives children a short graceful window.
Abort persists cancellation before pumps are aborted.

Session processes are spawned eagerly, one per selected repository, because that is the v1
product rule. Claude's paid startup work is independently lazy: `system/init` and the first
cache-write charge happen only when reporch injects that child's first turn. An unbriefed worker
therefore stays `spawning`, costs zero, and is treated as unchanged for the final completion
handoff. The budget shown before confirmation is the worst-case ceiling if every repository is
woken, not a reservation or predicted charge.

## Pure protocol, effectful supervisor

The core transition has the conceptual shape:

```text
(TaskState, Event) -> (TaskState, Vec<Action>)
```

Events carry their own timestamps and cumulative costs, so the transition does not read a clock.
It decides when a brief makes a recipient pingable, whether a requested ping is legal, when an idle
master or worker needs a nudge, whether a rate limit applies, and whether a violation aborts the
task.

Actions describe effects such as delivering a ping or surfacing a warning. The supervisor is the
interpreter. It polls append-only CLI request files and the durable brief index on every tick,
feeds new facts to the transition, sends approved wake-up lines through the recipient's stdin, and
persists the resulting task view. Display-only activity never enters the protocol.

This separation is why protocol property tests can generate event sequences without processes or
files, while actor tests can focus on ownership and persistence.

## Briefs, pings and the mailbox

Every spawned session receives the exact text in [`docs/protocol.md`](protocol.md) through
`--append-system-prompt`; the same bytes are copied into the task and shipped inside the binary
crate. The protocol is not written into a user's repository.

Brief bodies live in `briefs/*.md`; allocation and the chronological `briefs/index.json` update
happen under an advisory lock. A brief may carry a request from any repository to any other
repository. It does not itself wake a process.

Ping requests and status updates are append-only JSONL written by the agent CLI. The supervisor
consumes each line once, asks `reporch-core` whether delivery is allowed, records the decision, and
injects a small read-your-inbox turn over stdin. The worker-facing `reporch ping` refusal is part
of the enforcement surface, not just an error message.

## Persistence and recovery boundaries

`reporch-store` centralizes every path. `REPORCH_HOME` defaults to `~/.reporch`, and a task is
self-describing: operator brief, exact protocol and prompt, task state, mailbox, session streams,
structured result and merge checkpoint all live together.

JSON writes use a temporary sibling followed by rename. Shared counters and mailbox allocation
also take an advisory sidecar lock. Task menus are rebuilt from each task's `task.json`; there is no
second index that can drift.

There are three distinct recovery boundaries:

- Before session spawn, a preparation failure marks the task failed and leaves no paid process.
- During a run, cancellation, timeout or process failure preserves worktrees and recorded streams.
- During merge, `merge.json` checkpoints every confirmed step. A reopened merge verifies a
  recorded commit is still an ancestor before treating it as complete, and never reuses an old
  human confirmation to replay Git mutation.

## Git and merge ownership

All repository-changing operations shell out to the user's real `git`. That preserves worktree,
hook, merge and configuration semantics. Commands set `GIT_TERMINAL_PROMPT=0` and
`GIT_EDITOR=true`, so an unattended task cannot hang behind a credential or editor prompt.

The master proposes a merge order in `result.json`; the analyzed dependency graph validates it.
Disagreement becomes a visible warning, never a silent rewrite. Each step shows the actual
worktree diff and configured test command. Only the literal merge-screen `y` constructs the
`Confirmation` value accepted by the merge engine.

Many real sessions leave useful changes uncommitted. At that confirmation boundary reporch stages
the reviewed product changes, excludes the task's report artifact, creates a task-named commit,
rebases, tests and merges. A failure preserves the worktree for repair. Once every repository is
merged or explicitly skipped, teardown removes task worktrees and branches and compacts raw logs.

## Verification surfaces

- `cargo test --workspace --locked` covers pure transitions, properties, real processes, real
  temporary Git repositories, persistence concurrency and rendered terminal buffers.
- `fixtures/stream/` contains real Claude Code captures for parser and lifecycle behavior.
- `cargo xtask verify` adds formatting, Clippy, rustdoc warnings and a release build.
- `cargo xtask package` creates and verifies all six local archives without publishing.
- `cargo deny check` evaluates advisories, licenses, sources, duplicates and bans.

See [`docs/troubleshooting.md`](troubleshooting.md) for the operator-facing side of these failure
boundaries and [`docs/decisions.md`](decisions.md) for why the less obvious choices were made.
