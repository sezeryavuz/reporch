# Decisions

Every fork taken during the autonomous build, in order. Each entry: what the fork was, what was
chosen, what was rejected, and why. This is the audit trail for judgement calls the author did not
get to make.

---

## D001 — Keep the product specification under version control

**Fork.** The initial ignore rules excluded the product specification and repository conventions,
while the implementation process required every later change to remain reviewable against that
baseline. Either the ignore rules were intentional, or the written auditability requirement was.

**Chosen.** Track the durable specification and repository conventions.

**Rejected.** Leaving them ignored and committing only the README and license. That would make
every `> **Deviation**` callout invisible to review, and a specification outside the repository
cannot serve as its contract.

**Why.** Two independent prose statements of intent beat one line in a generated-looking ignore
file, and the failure mode of being wrong here is trivially reversible (`git rm --cached`).

---
## D002 — Phase 0 order: blast radius, not spike number

**Fork.** §15 numbers the spikes 1–10. Four of them (#1, #2, #8, #10) can invalidate the
architecture; the rest cannot.

**Chosen.** Ran them in blast-radius order and batched unrelated questions into single sessions
(#6, #3 and #4 shared one child; #10 and a re-run of #2 shared another).

**Rejected.** Running 1→10 in order, one session each.

**Why.** Session startup is the dominant cost (~$0.38 on Sonnet, measured), so batching questions
into one child is a direct saving. Answering the architecture-invalidating ones first meant that
if one had come back REFUTED, the redesign would have happened before anything else was built on
the assumption.

---

## D003 — Force compaction with `/compact` rather than by filling 100k tokens

**Fork.** Spike #10 asks whether the appended system prompt survives *auto*-compaction. Triggering
auto-compaction requires filling the context window; `--autocompact` accepts a minimum of 100k
tokens.

**Chosen.** Forced compaction with the `/compact` slash command, having just proved in spike #3
that slash commands are deliverable over stdin.

**Rejected.** Generating ~100k tokens of filler to trip the automatic threshold.

**Why.** Both paths emit the same `system/compact_boundary` event and, per the documented
"what survives compaction" table, both preserve the system prompt by the same mechanism — it is
not part of message history at all. The filler approach would have cost roughly ten times as much
to test the same code path. Recorded as a caveat: the `trigger` field read `"manual"`, so
`"auto"` was not exercised end to end.

---

## D004 — Liveness from the event stream, not the session registry

**Fork.** §4.1 designs liveness detection around the registry's `status` field. Spike #6 showed
`-p` sessions do not publish one.

**Chosen.** Derive liveness from the stream reporch already owns: a turn starts at the first event
after an injection and ends at `result`. `claude agents --json` is retained for orphan detection
only.

**Rejected.** Polling `~/.claude/sessions/<pid>.json`; parsing transcripts; inferring idleness
from a timer.

**Why.** It is more precise (exact, not sampled), it removes a dependency on an undocumented file
format, and it needs no polling loop. This is a case where the spike improved the design rather
than damaging it. Written into §4.1 as a Deviation.

---

## D005 — Keep `--add-dir` for the mailbox rather than `permissions.additionalDirectories`

**Fork.** `--add-dir` loads `.claude/skills/` and `.claude/agents/` from the target directory, with
live reload. `permissions.additionalDirectories` grants file access with no configuration pickup
and would be strictly safer.

**Chosen.** Keep `--add-dir`, as §7.1 and §12.2 specify.

**Rejected.** Switching to `permissions.additionalDirectories`.

**Why.** The only directory reporch adds is the task mailbox under `$REPORCH_HOME` — created and
owned by reporch, containing no `.claude/`. The exposure today is nil, `--add-dir` is the
documented path, and it is what the spec says. The constraint that actually matters was recorded
in §4.5 instead: `--add-dir` may never point at a user-controlled directory. Easy to reverse if
that constraint ever has to bend.

---

## D006 — Don't strip skills and MCP servers from spawned sessions for cost

**Fork.** Spawned sessions inherit the operator's whole global surface: 178 slash commands, 116
skills, 30 agents, 14 MCP servers. `--strict-mcp-config` and `--disable-slash-commands` remove
most of it.

**Chosen.** Measured it before deciding (spike 1b), then took `--strict-mcp-config` **for safety**
and left skills enabled.

**Rejected.** Stripping everything for cost; stripping nothing.

**Why.** The measurement killed the cost argument: removing all of it saved 14% of prompt tokens
(33,629 → 29,009), because the base system prompt and built-in tool definitions dominate and are
irreducible. What remains is a safety argument, and it only applies to MCP: fourteen MCP servers
running inside nine unattended `bypassPermissions` sessions is a materially larger attack surface
than the task requires, and reporch's sessions need none of them. Skills stay because they are
genuinely useful inside the user's own repositories and disabling them would degrade the product
to save nothing. Both are exposed in `config.toml` so the trade-off is the user's to change.

---

## D007 — `--effort max` + `{"ultracode": true}`, not `--effort ultracode`

**Fork.** Phase 0 found `--effort ultracode` is accepted, contradicting §4.3. It could express the
"max + ultracode" row of §5.6's ladder in one flag.

**Chosen.** `--effort max` together with `{"ultracode": true}` in the `--settings` payload.

**Rejected.** `--effort ultracode`.

**Why.** `--effort ultracode` pins effort to `xhigh`, but §5.6's top row requires **max** effort
*and* ultracode. The one-flag form cannot express it. Keeping effort and the ultracode toggle
orthogonal also matches the model §5.6 already describes, so the ladder code stays a pure function
of the effort enum with a boolean beside it.

---

## D008 — Treat budget exhaustion as session death, not as a pause

**Fork.** Spike #5 showed a session hitting `--max-budget-usd` exits with code 1. reporch could
auto-respawn with a raised ceiling, or stop and surface it.

**Chosen.** Mark the repo `dead`, surface it on the run screen, and offer a human-confirmed resume
with a raised ceiling.

**Rejected.** Automatically resuming with a higher ceiling.

**Why.** The budget ceiling is the mechanism §12.3 relies on to bound a runaway loop. A reporch
that automatically raises it the moment it binds has no such mechanism — the one brake that ends
in dollars would silently disable itself exactly when it fired. Spending more of the user's money
is a decision for the user.

---

## D009 — Read `.git/config` directly for the scan; shell out to `git` for everything else

**Fork.** CLAUDE.md says "shell out to the `git` binary. Do not link libgit2." The scan needs a
remote URL and a rough HEAD age for every repo on the machine — ~156 of them here.

**Chosen.** Parse `.git/config` and stat `.git/logs/HEAD` directly for the scan. Everything that
*changes* a repository — worktree add, branch, rebase, merge — shells out to real `git`.

**Rejected.** `git remote get-url origin` per repo (156 process spawns on the discovery path);
linking libgit2.

**Why.** The convention exists so that the user's config, hooks and credential helpers apply to
operations that matter, and so the build stays pure Rust. Neither concern is engaged by reading two
strings out of an INI file. 156 spawns would cost more than the entire scan (measured at 0.224s),
and the discovery screen is the first thing the user ever sees. This is the "cheap reads" exception
the convention already allows. Note the parser handles both `.git` shapes — a directory, and the
file containing `gitdir:` that a worktree has — because reporch creates worktrees itself.

---

## D010 — Jaro-Winkler for name similarity; nucleo-matcher for the interactive filter

**Fork.** §5.3 specifies "Directory name fuzzy-matches the project name | 0–60 | Jaro-Winkler via
`nucleo-matcher`". `nucleo-matcher` does not implement Jaro-Winkler. It is a *subsequence* matcher.

**Chosen.** Split the two jobs. Name similarity (the 0–60 ranking signal) uses a Jaro-Winkler
implementation written into `rank.rs`. The interactive `/` filter keeps `nucleo-matcher`.

**Rejected.** Using nucleo for both; pulling in a `strsim` dependency.

**Why.** The spec's own worked example disproves the specified approach: `mdlx-web` is offered as
the case name-similarity should catch, but `modulex` is not a subsequence of `mdlx-web` — no `o`,
no `u` — so nucleo scores it identically to a completely unrelated repo. A test asserts the
three-way ordering the spec intends (prefix > similar > unrelated), and it fails with nucleo.
Meanwhile subsequence matching is exactly right for the filter, where the user is typing an
abbreviation. Jaro-Winkler is ~40 lines with a prefix bonus that suits this problem particularly
well, since repos in one project overwhelmingly share a leading string. Written in rather than
added as a dependency because it is small, and reference values pin it in tests. Recorded as a
Deviation in §5.3.

---

## D011 — A spawned-but-never-pinged session is pingable

**Fork.** `RepoState::Spawning` means "the process is up but has not emitted `system/init`". The
intuitive reading is that such a session is not ready to receive attention.

**Chosen.** `Spawning` is both pingable and quiet.

**Rejected.** Waiting for `init` before considering a session reachable; sending a throwaway
warm-up turn to every session at spawn to force `init` early.

**Why.** `system/init` is lazy — it arrives at the start of a session's first *turn*, not when the
process starts (spike #1). So a worker that has never been pinged never emits `init` and stays in
`Spawning` forever: **the workers cannot be woken because they have never been woken.** The first
real end-to-end run hit exactly this — the master filed two briefs, the control plane decided both
recipients needed a ping, and delivered neither. A warm-up turn would fix it too, but it costs a
full session-startup charge per repo (~$0.38 on Sonnet, measured) to learn something reporch can
simply assume. The §4.4 assertion still runs on the first `init`, so the unverified window is one
turn, which is the earliest any check could happen given the laziness.

---

## D012 — `REPORCH_BIN` alongside `PATH`, because `PATH` does not survive

**Fork.** Sessions need to run `reporch`. Injecting reporch's directory into the child's `PATH` is
the obvious mechanism.

**Chosen.** Inject `PATH` **and** `REPORCH_BIN` with the absolute path, and name `REPORCH_BIN` in
both the injected system prompt and `docs/protocol.md`.

**Rejected.** `PATH` alone; installing a shim into the worktree (which would violate §7.3's
zero-diff rule); requiring reporch to be installed on `PATH` before it can be used.

**Why.** `PATH` alone does not work. Claude Code's `Bash` tool runs a login shell, which
re-sources the user's profile and rebuilds `PATH`, discarding the injected entry — observed
directly, with a tell-tale duplicated `~/.local/bin` at the front. Ordinary environment variables
are not rebuilt that way, so an absolute path in a variable survives where a `PATH` entry does
not. The failure this caused was total and silent: both workers ran `reporch brief read 0001`, got
`command not found`, correctly refused to act on a brief they could not verify (§12.2 working as
designed), and asked for help that no one was there to give. Telling sessions where the binary is
also stops them burning whole turns running `find / -iname reporch`.

---

## D013 — Recover stale worktree registrations in the product, not by cleaning the E2E fixture

**Fork.** The untouched §8 end-to-end command failed before spawning Claude because all three
committed fixture repositories still registered a task worktree whose directory had disappeared.
The branch-reuse path handled a leftover branch, but `git worktree add` refused it as “already
checked out” at the stale path. The immediate choices were to prune the fixture manually, reset it
before each example run, or make normal task creation recover from the crash state.

**Chosen.** Make `reporch-git::add_worktree` prune stale administrative entries before creating a
replacement, pinned by a real-git regression test that deletes a linked worktree directory without
deregistering it. **Rejected.** Special cleanup in `examples/e2e.rs`, because users can reach the
same state after a crash or manual directory removal; and one-off fixture cleanup, because it would
make the baseline command pass once without fixing retry safety. **Why.** §9.1 centralises
worktrees specifically for cleanup, and §11.3 requires teardown and recovery to be resumable. `git
worktree prune` removes only entries Git has already determined are stale and leaves live
worktrees untouched, so the recovery belongs at the product boundary that owns worktree creation.

**Correction after the first retry.** The historical worktree that blocked the baseline happened
to still exist under its temporary path, so this particular collision was not stale and pruning
correctly left it alone. The recovery above still closes the real vanished-directory case captured
by its regression, but repeatable proof runs require the separate D014 decision below.

---

## D014 — Derive the E2E branch from its output directory

**Fork.** `examples/e2e.rs` always created `TASK-0001` with slug `widget-version-field`. A fresh
output directory therefore still chose the same git branch as every previous proof run, and Git
refused to check that branch out while an earlier run's valid worktree remained alive. The choices
were deleting or stealing the earlier worktree, adding an operator-supplied run ID, or deriving a
stable ID from an argument the harness already requires.

**Chosen.** Append a deterministic short hash of the output directory to the example's task slug.
The same output directory retains the same branch and remains resumable; a different output
directory gets an independent branch. **Rejected.** Removing the historical worktree, because the
repository boundary forbids touching that external path and production code must never steal a
live branch; and an extra run-ID argument, because the output directory already names the run.
**Why.** This changes only the paid proof harness, keeps the §8 slug bound, preserves real task
resumption semantics, and makes the documented command safely repeatable without weakening Git's
protection of live worktrees.

---

## D015 — Keep session scratch files task-local and describe the boundary as guardrails

**Fork.** The first live Codex proof run reached the master, which then wrote two brief-body files
under `/tmp`. That contradicted §12.1–§12.2's statement that `--add-dir` limits a session to its
worktree and mailbox. The choices were treating this as harmless temporary-file behavior,
claiming the prompt should be enough, or tightening the mechanics while correcting the threat
model.

**Chosen.** Create `<task>/tmp`, inject it as `TMPDIR`, explicitly tell sessions to keep scratch
files there, and deny direct `Write`/`Edit` tool calls to `/tmp/**` and `/private/tmp/**`. Record a
§12.2 Deviation and state plainly that Bash under `bypassPermissions` is not OS-sandboxed.
**Rejected.** Ignoring temporary writes, because the operator's boundary applies to all writes;
and describing the new rules as containment, because scoped Claude permissions are deny
guardrails, not a replacement for a process sandbox. **Why.** The task mailbox is already the one
reporch-owned writable directory outside the worktree, so it is the natural scratch location.
Mechanical denies catch the observed path, while honest documentation prevents users from making a
stronger security inference than the implementation supports.

---

## D016 — Anchor a relative `REPORCH_HOME` before invoking Git

**Fork.** A repository-local E2E run used a relative home path. reporch wrote its project files
relative to its own cwd, but `git worktree add` resolved the same relative worktree target from
each source repository's cwd. The harness printed one path, created another, and session spawn
failed with `ENOENT`. The choices were documenting that `REPORCH_HOME` must be absolute, fixing
only the example, or normalising at the shared path boundary.

**Chosen.** `Paths::at` anchors a relative root to the caller's current directory immediately;
all derived project, task, mailbox and worktree paths are absolute thereafter. **Rejected.** An
example-only conversion, because users and tests may set the documented environment override to a
relative path; and rejecting relative paths, because they are useful for isolated local runs.
**Why.** One conversion in `reporch-store` removes cwd-dependent behavior from every subprocess,
keeps the public API unchanged, and matches what users mean when they export a path before reporch
starts changing process working directories.

---

## D017 — Treat the final worker acknowledgement as work owed by the master

**Fork.** In the first live Codex task, both workers read, implemented, acknowledged their briefs,
set themselves `done`, and stopped exactly as `docs/protocol.md` directs. The master had set itself
`waiting`. Because acknowledgements close outbound briefs rather than creating replies, §6.3's
old liveness predicate saw no outstanding work and never woke the master to write `result.md` or
call `reporch task finish`. The choices were requiring every worker to send a redundant reply,
having the supervisor finish automatically, or making completion a control-plane handoff.

**Chosen.** When every briefed worker is `done`, every never-briefed worker remains `idle`, no
brief is open, and the master is quiet, deliver a rate-limited and audited completion prompt to the
master. A repository that received or sent a brief cannot qualify as “unchanged” merely because
its runtime state fell back to `idle`. Delay the separate open-outbound reminder by one attention
interval so a freshly dispatched brief does not also produce a premature master reminder.
**Rejected.** Redundant reply briefs, because the mailbox already records the acknowledgement;
and automatic finish, because only the master can author the cross-repository result and §11.1
requires that explicit act. **Why.** This preserves the asymmetric attention protocol, keeps the
master as the sole completion authority, and turns the exact real-world terminal state into a
tested transition instead of another silent deadlock.

---

## D018 — Let Tokio own the console wait without adding a second event stream

**Fork.** Wiring project analysis and live sessions into the console required moving the
synchronous crossterm loop onto Tokio without blocking a runtime worker. The choices were enabling
crossterm's optional async `EventStream`, running blocking reads on a dedicated task, or polling
terminal input non-blockingly at the configured UI tick while selecting over background events.

**Chosen.** Build one multi-thread Tokio runtime at the console boundary; use
`crossterm::event::poll(Duration::ZERO)` only to drain already-ready input, then let
`tokio::select!` wait on the redraw interval and orchestration channels. Background jobs own
immutable requests and report transitions over unbounded channels; they never borrow `App`.
**Rejected.** A blocking reader task, because cancellation and terminal teardown would need a
second shutdown protocol; and `EventStream`, because 250 ms is already the product's configured
input/redraw latency and an extra dependency/feature adds no user-visible benefit. **Why.** The
runtime remains responsive, terminal reads never occupy a Tokio worker, and the immediate-mode UI
continues to have one owner and no locks across `.await`.

---

## D019 — Make one task actor own every live session pump

**Fork.** Moving task startup into the console created two tempting shapes: let `App` hold the
supervisor and poll each child itself, or spawn detached per-session tasks and keep only their
injectors. The first would mix rendering with orchestration and invite locks across awaits; the
second would orphan paid children when the parent task was cancelled because dropping a Tokio
`JoinHandle` detaches it.

**Chosen.** A background task actor owns the `Supervisor`, a `JoinSet` containing every session
pump, the session-event receiver, and the run-screen command receiver. Blocking worktree and hook
setup completes on `spawn_blocking` before any session starts. The UI receives immutable
`Started`/`View`/`Failed` events. User abort persists `cancelled` and aborts the `JoinSet`
immediately; normal completion closes injectors and gives children the verified graceful-exit
window. **Rejected.** UI-owned orchestration, because drawing must stay a projection; and detached
pumps, because an early return would leave Claude processes spending with no owner. **Why.** A
dropped `JoinSet` aborts its members, each pump owns a `ChildTransport` with `kill_on_drop`, and the
normal path still explicitly shuts down and reaps every child. This makes ownership, cancellation
and the no-orphan guarantee structural rather than conventional.

---

## D020 — Make the paid proof exercise the console's task actor

**Fork.** After the console gained a live task actor, `examples/e2e.rs` still duplicated session
spawn, kickoff, supervision and shutdown. Keeping that verbose reference made its transcript easy
to read, but a green paid proof could stop saying anything about the path users actually run.

**Chosen.** The example builds its deterministic fixture project and capped `TaskRunRequest`, then
consumes the same `Preparing`/`Started`/`View`/`Failed`/`Stopped` stream as the console. It retains
human-readable progress, mailbox, diff and artifact reporting. `Started` is emitted only after the
kickoff reaches the master, so both consumers may treat it as a real live-task boundary.
**Rejected.** A shared lower-level helper with two orchestration loops, because ownership and
shutdown behavior would still be duplicated; and deleting the example, because it is the only
deliberately paid, inspectable proof. **Why.** The next capped E2E run will verify the console's
actor rather than a parallel implementation, while ordinary tests remain unable to spend money.

---

## D021 — Pause attention, never observation or safety

**Fork.** The §5.8 footer promised `p pause`, but a long-lived `stream-json` Claude process has no
verified suspend/resume primitive. reporch could send a Unix stop signal, close stdin and attempt
resumption, or define pause at the control-plane boundary it actually owns.

**Chosen.** Pause queues every master-requested and control-plane ping before stdin delivery while
the actor continues consuming session output, accounting spend, processing state transitions,
and enforcing the wall-clock ceiling. The protocol state still rate-limits queued pings; resume
delivers each once, records it at delivery time, and immediately evaluates liveness. **Rejected.**
`SIGSTOP`, because it is Unix-specific and could freeze a child while leaving provider work or
network state ambiguous; and closing/restarting the session, because that turns a reversible UI
control into process death, possible spend, and unproven resumption. **Why.** Attention injection
is the scheduler behavior reporch owns. Observation and safety are the parts it must never pause.

---

## D022 — Snapshot reviewed session edits at the confirmation boundary

**Fork.** The first task to reach `completed` left real changes uncommitted in all three
worktrees. The existing merge engine rebased and merged branch commits only, so it would report a
successful no-op. reporch could require sessions to commit, refuse dirty worktrees, or create the
commit when the user approves that repository.

**Chosen.** A `y` confirmation first stages the reviewed worktree and creates a task-named commit,
then rebases, tests and merges. An untracked root `result.md` is explicitly unstaged because its
authoritative copy belongs to task history. Commit-hook and missing-identity failures block with a
repair instruction. **Rejected.** Prompting sessions to commit, because compliance would remain
advisory and commit history is not part of their task; and refusing dirty trees, because every
proven session followed the protocol and still produced one. **Why.** This binds Git mutation to
the existing human review boundary and makes the merge engine operate on the work that actually
exists without silently adding task metadata to the product.

---

## D023 — Enforce structured reports prospectively and derive only provable historical facts

**Fork.** §11.1 claimed `result.md` was generated against a schema, but the kickoff prompt did not
show one and `task finish` accepted any non-empty prose. The completed live proof consequently has
a useful narrative report but no machine merge order, changed flags, test results or notes.

**Chosen.** New kickoff prompts include the complete JSON shape. `task finish` parses bare or
fenced JSON, validates every repository and changed-repo field plus merge-order coverage, writes
`result.json`, and only then requests completion. For older narrative tasks, load the prose summary
and derive changed flags and file counts from Git with a visible warning; leave merge notes, tests
and risks empty. **Rejected.** Rejecting historical tasks, because their Git diffs are real; and
asking another model to reconstruct the report, because that spends money and could invent facts
at the most consequential boundary. **Why.** Future merges are deterministic, while compatibility
is conservative and evidence-based.

---

## D024 — Close completed sessions before human merge review

**Fork.** §11.2 assumed a conflict could be handed back to the repository's still-live session.
In the actual actor, completion closes every injector and reaps every child; keeping paid
`bypassPermissions` sessions alive for an unbounded period while a human reviews repositories
would violate the no-orphan ownership model.

**Chosen.** Completion always shuts sessions down. A rebase, test or checkout-state failure is
shown in the merge screen with the worktree preserved; the user repairs it and retries the
human-confirmed step. **Rejected.** Holding children through merge review, because review can last
hours or survive a laptop close; and automatic `--resume`, because resumption has not been proven
and can spend beyond the original completed task. **Why.** The safe process lifetime is bounded by
the task actor. Conflict assistance can be revisited only after capped, explicit resume exists.

---

## D025 — Rebuild task menus from the persisted task records

**Fork.** The project menu needs live running/completed/history counts after restarts, including
failed and cancelled runs. The choices were maintaining another project-level index, inferring
state from directory names and marker files, or treating each task's existing `task.json` as the
catalogue record.

**Chosen.** Enumerate task directories when a project opens, parse every readable `task.json`,
sort by numeric task id newest-first, and derive menu buckets from `TaskStatus`. A malformed task
is surfaced as a warning while the remaining history stays available. **Rejected.** A second
index, because every task transition would need a transactional two-file update; and directory or
marker inference, because neither contains the complete lifecycle state. **Why.** `task.json` is
already the atomic source of truth in §8.2. Rebuilding a small local view avoids drift and makes
partial corruption visible instead of turning it into an empty project.

---

## D026 — Re-analysis refreshes discovered facts, not user-owned settings

**Fork.** Re-running project analysis can rebuild the dependency graph and repository profiles,
but the ordinary new-project path also seeds consent, budget, carry-over rules, hooks and the next
task id. Reusing it literally would silently reset later settings edits and could reuse a task id.

**Chosen.** Supply the prior `ProjectBase` to the analysis job and preserve `created_at`, all
settings, and `next_task_id`; replace the overview, repository facts, graph, profiles and
`analyzed_at`. **Rejected.** Re-seeding settings from analysis, because those values became
explicit user decisions after project creation; and patching only the graph, because repository
paths/default branches/profile facts may be the stale input that prompted re-analysis. **Why.**
The settings action now means “read the repositories again” without unexpectedly undoing project
configuration or corrupting task allocation.

---

## D027 — Checkpoint merge decisions and verify completed commits on resume

**Fork.** A per-repository merge can be interrupted after Git succeeds, after `merge.json` is
written, or during teardown. Rebuilding a fresh plan would ask for a second confirmation and might
try to merge the same branch again; blindly trusting the checkpoint could hide work if the user's
checkout later moved.

**Chosen.** Persist after every merge or skip. On reopening a completed task, restore terminal
steps by repository and branch; accept a recorded merged commit only when Git proves it is still
an ancestor of the source checkout. Advance unchanged and already-terminal steps without creating
a synthetic confirmation. Persistence and teardown failures enter a separate `r` recovery path
that never replays the Git merge, while failed rebase/test/merge steps remain current for an
explicit `y` retry or a separately confirmed skip. **Rejected.** Re-running every step, because a
confirmation is single-use intent rather than permission for repeated mutation; and accepting
`merge.json` without repository verification, because local history may have been rewritten.
**Why.** The merge flow is resumable at its actual failure boundaries while the `y` key remains
the only constructor of merge authority.

---

## D028 — Keep release discovery advisory and infer upgrades from the executable path

**Fork.** The top-level update action needs an install-specific command, but v0.1 has no installer
receipt and GitHub may be offline or have no published release. The choices were adding persistent
install metadata, probing package managers, or inferring the method from the resolved executable
layout with a universally valid shell-installer fallback.

**Chosen.** Query GitHub's latest-release endpoint on a bounded background job; turn HTTP, DNS,
timeout and parse failures into a non-fatal notice. Detect Cargo, Homebrew and npm from canonical
binary paths, then fall back to cargo-dist's release installer command. Browser opening likewise
runs off the render loop and passes the URL as one process argument to `open` or `xdg-open`.
**Rejected.** Package-manager probes, because they are slow, can themselves be absent, and still
cannot identify a manually copied binary; and a stored receipt, because none exists for already
installed copies. **Why.** Update checks never delay or disable orchestration, the common install
paths get the shortest correct command, and the fallback works without pretending detection was
certain.

---

## D029 — Make xtask package locally, and make publishing unrepresentable

**Fork.** The hardening brief prescribed `cargo package --dry-run`, but Cargo 1.95 rejects that
flag. The choices were omitting package verification, shelling out to an older toolchain, or using
Cargo's actual separation: `package` creates and verifies a local archive while `publish` uploads
it.

**Chosen.** `cargo xtask package` invokes one `cargo package --locked --workspace --exclude xtask`
batch. Cargo's temporary local registry can then resolve the six mutually dependent, unpublished
crates and verify each archive. A command-plan test asserts workspace coverage and rejects the
word `publish`; the xtask dispatcher exposes no arbitrary passthrough. **Rejected.** Toolchain
downgrading, because the repository intentionally verifies its pinned current toolchain; separate
package invocations, because crates.io cannot resolve unpublished sibling versions; and
`--no-verify`, because crate compilation is the point of this gate. **Why.** The helper proves the
artifacts without network publication, and its executable vocabulary cannot express the
prohibited action.

---

## D030 — Separate the pinned development toolchain from the MSRV gate

**Fork.** Contributors need deterministic `rustfmt` and Clippy output, while the manifests claim
Rust 1.85 compatibility. Pinning the repository to 1.85 would make the minimum version the daily
toolchain; pinning only the current compiler would leave the compatibility claim untested.

**Chosen.** Pin local and matrix checks to Rust 1.95.0 with the `clippy` and `rustfmt` components,
and run a distinct locked `cargo check --workspace --all-targets` job on Rust 1.85.0. Exercise
Linux and macOS on both GitHub-hosted x86_64 and arm64 runners, and keep supply-chain policy in a
separate least-privilege job. **Rejected.** Cross-compiling from one host, because it does not run
tests against either operating system; and using a floating `stable`, because Clippy and formatter
changes would make contributor results time-dependent. **Why.** The pinned toolchain makes the
normal quality gates repeatable, while the dedicated job tests exactly the weaker promise made to
downstream users.

---

## D031 — Constrain transitive upgrades that silently exceed the MSRV

**Fork.** The first real Rust 1.85 build found that a normal lockfile refresh had selected
`instability` 0.3.13 and `ignore` 0.4.33, both requiring Rust 1.88. `ignore` 0.4.30 also used
let-chains without declaring its compiler requirement. The choices were raising the advertised
MSRV, leaving a knowingly red compatibility job, or retaining compatible releases.

**Chosen.** Lock `instability` to 0.3.10 and `globset` to 0.4.19, and require exactly `ignore`
0.4.29 in the publishable scan crate. Keep the independent Rust 1.85 CI job so future dependency
updates must prove compatibility. **Rejected.** Raising the MSRV, because none of reporch's own
code or behavior needs Rust 1.88; and relying only on dependency `rust-version` metadata, because
the failing `ignore` release did not declare it. **Why.** The declared floor should describe the
product's needs, and an actual compiler run is stronger evidence than registry metadata.

---

## D032 — Treat the transitive `paste` notice as a reviewed exception

**Fork.** `cargo deny check` found no vulnerability, but failed because Ratatui 0.29 depends on the
archived `paste` procedural-macro crate. The RustSec entry reports unmaintained status and no safe
upgrade; replacing the terminal stack during release hardening would be a disproportionate product
change.

**Chosen.** Ignore only `RUSTSEC-2024-0436` with its reason in `deny.toml`, while leaving all
vulnerability and unsoundness advisories at cargo-deny's failing defaults. Keep duplicate versions
visible as warnings, reject wildcard dependencies and unknown sources, and ban accidental OpenSSL
adoption because release networking already uses rustls. **Rejected.** Disabling unmaintained
checks globally, because new abandonment notices should be reviewed; and upgrading Ratatui during
hardening, because that changes the rendering foundation without fixing a security defect. **Why.**
The exception is narrow, auditable and removable when the UI stack advances, while every higher
severity supply-chain signal remains a gate.

---

## D033 — Initially retain build prompts outside the product front door

**Fork.** The autonomous-build prompts explained how implementation work was commissioned, but
their root placement presented stale build instructions beside product entry points. They could
be deleted, left at root, or retained under documentation. The same review also asked whether
funding and citation metadata belonged in 0.1.0.

**Chosen at the time.** Move both prompt records with history to a dedicated documentation
directory; they remained auditable without looking like user documentation. Skip `FUNDING.yml`
because no funding account or sponsorship program exists, and defer `CITATION.cff` until there is
a real release/tag and stable bibliographic identity rather than inventing metadata. **Rejected.**
Deleting the prompts, because they explain the decisions and milestones that produced the current
tree; and leaving them at root, because new users should meet the README, contribution guide and
security policy first. **Why.** Provenance remains available in the appropriate layer, while the
root describes the software as it exists. The `.DS_Store` named by the hardening brief was already
absent when inspected, so there was no filesystem object to delete.

---

## D034 — Document bounded manual recovery instead of pretending resume is wired

**Fork.** The spawn layer can rebuild a Claude command with `--resume`, but the live task actor
owns a fixed injector map and session `JoinSet`; there is no console action that replaces one dead
child or asks for a new ceiling. The choices were adding that architectural mutation during
documentation hardening, silently describing the planned control as real, or making the alpha's
recovery boundary explicit.

**Chosen.** Surface budget/crash death, preserve the stream and worktree, and document manual
completion or a newly budgeted replacement task. Add a Deviation that names the exact work a safe
resume control still requires. **Rejected.** Automatic respawn, because it would neutralize the
spend brake; and a hurried interactive respawn, because cumulative cost across a restored
transcript and injector replacement need a separately capped real proof. **Why.** A missing
recovery convenience is safer than a control that can orphan a child, double-count spend or spend
past the user's decision. The docs now describe the behavior an operator can actually invoke.

---

## D035 — Use durable model aliases instead of probing account entitlements

**Fork.** Claude Code 2.1.228 documents Fable 5 and native 1M-context choices, but Fable access and
usage-credit eligibility vary by account. The choices were a direct `fable` option, scraping the
interactive `/model` picker or starting a paid probe, or presenting Claude's durable aliases and
letting session startup validate the user's account.

**Chosen.** Keep `opus` as the non-surprising default; add `best`, `opus[1m]`, `sonnet`,
`sonnet[1m]`, and `haiku`. Describe `best` as Fable 5 where available with Claude's documented
Opus fallback, and warn on the 1M options that availability and usage credits vary. **Rejected.**
A direct `fable` option, because it fails for organizations without access; picker scraping,
because the interactive UI is not an API; and a paid entitlement probe, because discovery should
not spend money. **Why.** Durable aliases survive concrete model-version turnover, expose the
current high-end and long-context choices, and do not make a headless task's billing behavior more
surprising by changing the default.

---

## D036 — Treat each resumed Claude process as a new cost epoch

**Fork.** The 2.1.228 resume spike restored the same session id and transcript but restarted
`result.total_cost_usd`; the original process ended at `$0.1035003` and its replacement reported
`$0.0199138`. The future respawn control could replace the old cumulative value, assume Claude
will eventually restore cross-process totals, or persist each process epoch and add them.

**Chosen.** Define cumulative cost as process-local. A future respawn must freeze the dead child's
last value, start a new epoch at zero, and report their sum at task level. **Rejected.** Replacing
the value, because displayed spend would move backwards; and relying on undocumented restoration,
because the real CLI contradicts it. **Why.** Session identity describes transcript continuity,
not billing-counter continuity. v0.1 needs no code change because it does not expose respawn, but
the constraint is now explicit in the architecture required to add that control safely.

---

## D037 — Expose strict Fable choices after the live picker advertised them

**Fork.** D035 initially omitted direct Fable aliases in favor of `best`, but the 2.1.228 `/model`
response explicitly advertised both `fable` and `fable[1m]`, and the hardening contract asks that
new and long-context variants be visible. The choices were retaining only the portable fallback
or exposing strict account-dependent choices alongside it.

**Chosen.** Add `fable` and `fable[1m]` as explicit opt-ins whose descriptions warn that they
require organization access; retain `best` as the safe Fable-to-Opus fallback and `opus` as the
default. **Rejected.** `default`, because reporch promises one explicit task model rather than an
ambient user setting, and `opusplan`, because its hybrid plan/execute behavior violates that same
single-model rule. **Why.** The selector now represents every relevant current family and
long-context choice without hiding strict Fable from users who have access or changing behavior
for users who simply accept the default.

---

## D038 — Keep the five-level launch ladder despite `/effort auto`

**Fork.** The 2.1.228 headless `/effort` usage string includes `auto`, but the CLI launch flag
rejects `--effort auto` and lists only `low`, `medium`, `high`, `xhigh`, and `max`; ultracode is a
separately accepted compatibility value. The product could add an adaptive sixth row or retain its
deterministic master/worker ladder.

**Chosen.** Retain the five launch-time levels and independent ultracode toggle. Current official
documentation and an invalid-value control confirm that `--effort ultracode` and the session-only
`ultracode` setting remain supported. **Rejected.** `auto`, because reporch launches with a flag
that rejects it and cannot derive a worker's "one step below" setting from an account-dependent
model default. **Why.** Every displayed choice is executable at spawn and the cost relationship
between master and workers stays explicit.

---

## D039 — Keep process startup eager because paid startup is already turn-lazy

**Fork.** The hardening brief suggested spawning workers only when a task's predicted involvement
set includes them, based on the measured first-turn cache-write cost. The choices were adding a
dormant repository state and dynamic child/injector creation, or retaining one idle process per
repository while relying on Claude's lazy `system/init` behavior.

**Chosen.** Keep eager process ownership and inject only the master plus workers that receive
briefs. An unbriefed process never takes a turn and therefore incurs no Claude charge. Treat a
never-briefed `spawning` worker as unchanged for the completion handoff; a regression test covers
the multi-repo/narrow-work shape that exposed this distinction. **Rejected.** Lazy process
creation, because it would complicate abort, orphan, injector, PID and spawn-failure handling
without avoiding the paid first-turn event. **Why.** The actor retains a fixed, auditable set of
children and instant wake delivery, while actual spend is already proportional to repositories
that receive work. The task budget remains an honest worst-case ceiling rather than expected
spend.

---

## D040 — Let the graph elect and let the master classify involvement

**Fork.** §5.7 specifies a paid one-shot model election that also predicts likely and unlikely
repositories, but the console uses the analysed dependency graph directly and asks the master to
plan involvement in its kickoff turn. The choices were wiring the extra classifier now or making
the deterministic behavior explicit.

**Chosen.** Rank upstream dependency reach deterministically, explain the result, let the human
change it, and give every analysed repository purpose to the master for task-specific dispatch.
**Rejected.** The one-shot classifier, because it adds a hidden paid turn, introduces another
model-generated failure mode before task start, and cannot safely exclude later-discovered work;
it also provides no cost benefit under D039. **Why.** Dependency direction is the durable signal
the election actually needs, while the long-lived master has the task context and remains free to
involve any repository.

---

## D041 — Persist cumulative `modelUsage`, not per-turn `usage`

**Fork.** Claude result events contain a per-turn `usage` object and a cumulative `modelUsage`
map, while reporch previously parsed the former and discarded both. Token observability could sum
turn objects, retain only the latest turn, or carry the cumulative per-model counters alongside
cost.

**Chosen.** Sum the latest `modelUsage` entries across models within one child, assign that
cumulative snapshot to its repository, persist input/output/cache-read/cache-creation tokens in
`task.json`, and show exact per-repository cost and token totals in history. **Rejected.** Summing
turn `usage`, because command-only turns and model switching make it easy to double-count or miss
tokens; and log-only observability, because merge compaction deliberately deletes raw streams.
**Why.** The same assignment rule already proven for cost now applies to tokens, older task files
deserialize the additive field as zero, and the useful accounting survives log retention.

---

## D042 — Verify analysis on repository-local Git repos

**Fork.** The hardening brief asks for analysis against several real repositories, while the task
boundary forbids touching any other repository. The choices were to inspect unrelated checkouts
outside this workspace, treat unit fixtures as sufficient, or run the actual console over the
three repository-local Git repos already used by the paid end-to-end proof.

**Chosen.** Run release-console analysis over `scratch/e2e/contracts-{api,client,docs}` with Haiku,
retain the generated artifacts in the ignored proof home, and commit a faithful summary including
the graph's missed reciprocal docs edge. **Rejected.** External checkouts, because that would
violate the explicit write/read scope; and unit evidence alone, because it cannot exercise model
output or async console progress. **Why.** The repositories are small but genuine and encode a
cross-language contract, so the run closes the integration unknown without expanding authority;
the documented limitation remains honest about what it does not prove.

---

## D043 — Complete only after the master's enclosing turn ends

**Fork.** The master writes `finish-requested` from a Bash tool inside its final Claude turn. The
live console proved that completing as soon as the supervisor sees that marker closes session
stdin before the turn's `result` event, leaving the master `working` and omitting its final cost
and tokens. Completion could stay marker-immediate, or wait for transport-level turn completion.

**Chosen.** Track open turns independently of model-reported repository status, keep the task
active while the master's final turn is open, and apply completion immediately after its
`TurnEnded` event records cumulative telemetry. **Rejected.** Inferring from `RepoState`, because
the master may set itself `done` before the same Bash tool calls `task finish`; and accepting
truncated telemetry, because completed history must not systematically omit the most important
session. **Why.** The validated report remains the authority to complete, but the process result
is the authority for billing and token accounting. Waiting for that already-in-flight boundary
costs no extra model turn and permits graceful shutdown.

---

## D044 — Verify unpublished sibling crates from their generated archives

**Fork.** Cargo's built-in `cargo package --workspace` verification rewrites path dependencies to
registry dependencies before compiling each crate. Because reporch's six publishable crates share
the unpublished `0.1.0` version, that check selected older crates.io siblings and both hid the
archive contents and produced incompatible types. The choices were to accept source-workspace
verification, add `--allow-dirty` or publish-order assumptions, or reconstruct the local crate set
from the archives Cargo actually generated.

**Chosen.** Create all six archives in one locked, non-publishing `cargo package --no-verify`
pass; extract each archive into a clean directory under `target/package`; then compile every
extracted crate and all of its targets offline, patching only its reporch dependencies to the
other extracted archives. Keep the recorded update-check response inside the CLI crate so its
tests are package-self-contained. **Rejected.** Compiling the source tree alone, because it cannot
catch missing included files; `--allow-dirty`, because a release gate should verify committed
content; and registry fallback, because an older same-version crate is not the artifact being
released. **Why.** The gate now checks exactly what would be uploaded, preserves locked resolution
for archive creation, performs no publication, and can bootstrap the first coordinated release
without pretending those sibling versions already exist on crates.io.

---

## D045 — Publish the engineering record, not the private build diary

**Fork.** D033 retained autonomous-build prompts, handoff reports and progress notes for internal
auditability. Before the first public release, those files could remain in the repository, be
partially redacted, or be removed while the product specification, empirical spike record and
decision log stayed public.

**Chosen.** Remove the private build prompts, AI handoff reports, transient plans, progress diary
and the superseded source brief. Keep `enhanced_brief.md` as the complete public specification,
`docs/decisions.md` as the judgement-call trail, and `docs/spikes.md` plus sanitized fixtures as
the reproducible evidence. Replace the pre-public commit graph with one reviewed initial-release
commit, after preserving the original refs in an ignored local Git bundle.

**Rejected.** Publishing the entire build diary, because it adds stale instructions, machine
identity and conversational provenance without helping users operate or review the software;
and deleting the engineering record wholesale, because the protocol and safety decisions remain
important review material.

**Why.** A public repository should expose product intent, implementation constraints and
verifiable evidence—not private orchestration transcripts. The retained documents explain every
load-bearing choice without leaking local context, while the local bundle keeps the rewrite
recoverable before anything is pushed.

---

## D046 — Sanitize real captures without turning them into mocks

**Fork.** The stream fixtures are real Claude Code captures and therefore included local paths,
random session identifiers, hook output and the operator's complete extension inventory. They
could be published verbatim, replaced with hand-authored minimal events, or transformed while
preserving the behavior the tests exercise.

**Chosen.** Deterministically replace machine and session identity, blank hook content, and remove
operator-specific plugin, skill, agent and MCP inventory. Preserve event order and shape, built-in
tool names needed by the deny-rule assertion, costs, tokens, compaction boundaries and terminal
outcomes. Add an explicit sanitization note to the metadata and spike record.

**Rejected.** Publishing raw captures, because environment fingerprints add no reproducibility;
and replacing them with synthetic mocks, because real protocol nesting and unknown event types
are the fixtures' reason to exist.

**Why.** The resulting corpus remains a replay of observed Claude Code behavior while exposing no
personal workstation details. Existing parser and session tests prove the transformation retained
the load-bearing evidence.

---

## D047 — Use one public owner identity across every release surface

**Fork.** The prepared source tree still pointed at its original organization in crate metadata,
installers, update checks, issue templates and release automation, while the public repository is
owned by `sezeryavuz`. Those references could remain as historical provenance, be changed only in
the README, or be normalized throughout the product and pipeline.

**Chosen.** Use `sezeryavuz/reporch` for source, update and release URLs;
`sezeryavuz/homebrew-tap` for the intended tap; and Sezer Ufuk Yavuz as the MIT copyright and crate
author. Use `hey@sezeryavuz.com` only on the security and conduct surfaces that need a public
contact. Replace organization names in generic scanner examples with `example-org`.

**Rejected.** Mixed ownership, because installed binaries would check the wrong release feed and
future release jobs would publish to the wrong tap; and embedding an email in every metadata
surface, because one deliberate public contact path is sufficient.

**Why.** Ownership is executable metadata here, not cosmetic branding. A single identity keeps
badges, package manifests, update discovery, installers, issue links and release automation
consistent with the actual public repository.
