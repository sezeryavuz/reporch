# CLAUDE.md — reporch

## What this repo is

**reporch** is a Rust terminal application that runs one development task across every repository
of a multi-repo project, by orchestrating a Claude Code session inside each one. The binary is
`reporch`, aliased `rp`.

**Current state: working pre-release alpha.** The six-crate implementation, console, protocol,
merge flow and release configuration exist and are covered by real Git/process tests. No release
has been cut and no package or installer has been published.

## Read before doing anything

| File | What it is |
| :--- | :--- |
| **`enhanced_brief.md`** | **The spec. The contract. Read it fully before writing code.** Numbered sections; cite them (`§6.3`) in commits and PRs. |
| `docs/decisions.md` | The judgement-call record. Continue the D0NN numbering when implementation reality leaves more than one defensible path. |
| `docs/spikes.md` | Empirical Claude Code findings and the sanitized capture inventory behind them. |

If a change contradicts `enhanced_brief.md`, **update the brief in the same change** with a
`> **Deviation**` callout explaining why. A spec that silently drifts from the code is worse than
no spec.

## The four ideas that must not be broken

Everything else is negotiable. These are not.

1. **Briefs are content; pings are attention.** Anyone may write a brief. Only the master pings a
   worker. Workers never ping anyone. The control plane pings on their behalf, rate-limited and
   logged. (§6)

2. **The protocol is enforced in code, not in prompts.** Sessions get `SendMessage` and
   `ListAgents` denied, and all inter-repo traffic goes through the `reporch` CLI, which reads
   `REPORCH_ROLE` from the environment and refuses violations. Never "solve" a protocol bug by
   adding a sentence to the system prompt. (§6.2, §7)

3. **Never write into the user's repository.** The protocol goes in via `--append-system-prompt`,
   config via `--settings`. No `CLAUDE.md` edits, no committed files, no diff on the task branch
   that reporch put there. Anything that genuinely must land in a worktree goes in
   `.git/info/exclude`. (§7.3)

4. **`crossSessionInbound: "accept"` on every spawned session.** Without it, a bypassing receiver
   holds every inbound message pending an approval dialog that a `-p` session cannot display, and
   the whole product deadlocks silently. Assert it at spawn. (§4.4)

## Architecture

```
crates/
  reporch/          binary — TUI + agent CLI, one executable
  reporch-core/     domain + protocol state machine. NO I/O. This is where tests live.
  reporch-store/    on-disk layout, atomic writes, locking, schema migration
  reporch-claude/   spawning, stream-json codec, session registry, doctor
  reporch-git/      worktrees, branches, merge
  reporch-scan/     repo discovery and ranking
```

`reporch-core` must stay pure. The protocol transition is
`(TaskState, Event) -> (TaskState, Vec<Action>)` — testable in milliseconds against a recorded
event log, with no Claude process and no filesystem. If a protocol rule is hard to test, it is in
the wrong crate.

## Conventions

- **Errors:** `thiserror` in library crates, `anyhow` only at the binary boundary. No `unwrap()` or
  `expect()` outside tests and `main`.
- **Logging:** `tracing`, to `~/.reporch/logs/reporch.log`. **Never to stdout or stderr** — it
  corrupts the TUI and poisons `--json` output that agents parse.
- **Git:** shell out to the `git` binary. Do not link libgit2. Users expect their config, hooks and
  credential helpers to apply, and a pure-Rust build matters for cross-compiled releases.
- **Writes:** every file under `~/.reporch/` is written temp-then-rename. Nine sessions share that
  directory. `briefs/index.json` needs an advisory lock around allocate-and-write.
- **Schemas:** every persisted file carries `schema_version`. Adding a field is fine; changing the
  meaning of one requires a migration.
- **Time:** UTC, RFC 3339, everywhere.
- **Async:** never hold a `std::sync::Mutex` across `.await`. Never block the runtime.

## The agent CLI is a product surface

`reporch brief`, `reporch ping`, `reporch status` and friends are read and invoked by a language
model with no memory of the documentation. Treat them accordingly:

- `--json` on everything readable. A model parsing prose is a model getting it wrong.
- **Every error message teaches the correct alternative.** `reporch ping` refusing a worker must
  print the exact commands that *would* work. See §6.2 for the required shape.
- Exit codes are meaningful: `0` ok, `2` protocol violation, `3` not found, `4` state conflict.

## Testing

- The `reporch-core` state machine is the highest-value test surface. Property-test the
  transitions; assert ping decisions against recorded event logs.
- Generated artifacts — the appended system prompt, `task.json`, brief frontmatter, the kickoff
  prompt — are `insta` snapshots. They change often and every change should be reviewed.
- The stream-json parser is tested against **committed fixture streams**, captured from a real
  session (spike #1 in §15). Never mock what a fixture can record.
- `assert_cmd` + `predicates` for the agent CLI, including the refusal paths.

## Commands

```bash
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo insta review          # after snapshot changes
```

## Platform facts worth not re-deriving

Measured on Claude Code **2.1.226** in Phase 0. Every claim here has an experiment behind it in
`docs/spikes.md`. Re-run those spikes on minor version bumps.

- **Liveness comes from the event stream, not the registry.** An app-spawned `-p` session appears
  in `claude agents --json` but reports **no `status`** field, and its `kind` is `"interactive"`.
  reporch owns each child's stdout, so a turn starting (first event after injection) and ending
  (`result`) are observed exactly. `claude agents --json` is used for **orphan detection only**.
  `~/.claude/sessions/<pid>.json` is undocumented; read defensively, never depend on it. Its
  `entrypoint` field (`sdk-cli` vs `cli`) is the only reliable "is this ours" discriminator.
- **`result.total_cost_usd` is cumulative for the session.** Per-turn cost is the delta. Never sum.
- **Cost is a startup charge, not a per-turn charge.** ~$0.385 for a Sonnet session's first turn
  (a ~30–55k-token system prompt gets cache-written), ~$0.018 thereafter. Nine sessions ≈ $3.50
  before any work happens.
- **`--max-budget-usd` overshoots and then kills the process.** $0.05 ceiling → $0.186 spent,
  `terminal_reason: "budget_exhausted"`, exit 1. Treat it as session death, not a pause.
- **Deny rules genuinely remove tools.** `{"permissions":{"deny":["SendMessage","ListAgents"]}}`
  takes `system/init.tools` from 60 to 58 and the two names vanish. Bare tool name = removed from
  context; a scoped rule like `Bash(rm *)` only blocks the call. §6.2 rests on this.
- **Assert `system/init` after spawn.** `-p` mode *silently ignores* a settings payload that fails
  validation, so passing the flag proves nothing. Read back `tools[]` and assert the two names are
  absent. Note `system/init` fires **once per turn**, not once per process.
- **`isReplay: true`** marks a `--replay-user-messages` echo — the delivery ack for an injected
  ping, and the only way to tell it from a `tool_result`, which shares the `user` type.
- **`--append-system-prompt` survives compaction** (verified through `/compact`, 35,833 → 7,955
  tokens). No re-injection timer needed. `system/compact_boundary` reports the boundary.
- **Never pass `--bare`.** It unbinds the inbox socket and hides the session from the agent list —
  and the docs say it will become the `-p` default in a future release.
- **`--resume` restores the transcript, not the flags.** Re-pass the entire flag set on respawn.
- Effort levels are `low, medium, high, xhigh, max` — **and `ultracode` is also accepted** by
  `--effort` (it pins effort to `xhigh` and turns ultracode on). For "max + ultracode" use
  `--effort max` plus `{"ultracode": true}` in `--settings`. (§4.3)
- `--session-id` lets reporch assign the UUID *before* spawning, so `task.json` is correct with no
  discovery race. Verified to survive `SIGKILL` + `--resume` with full context.
- Cross-session messaging requires Claude Code `>= 2.1.224`, macOS or Linux, and a first-party
  provider. Not on native Windows, Bedrock, Claude Platform on AWS, Google Cloud Agent Platform,
  or Microsoft Foundry. `crossSessionInbound` has **inverted precedence** — a stricter value in a
  repo's own settings beats `--settings`.

## Safety, non-negotiable

reporch runs sessions in `bypassPermissions`. That was a deliberate choice by the author; §12 of
the brief is what makes it survivable, and none of it is optional.

- Consent is explicit, per project, never a default.
- `--add-dir` grants the worktree and the task mailbox. Nothing else.
- Deny rules: no `git push`, no `gh pr create`, no writes to `~/.claude/**`, `~/.ssh/**`, or
  reporch's own config.
- **Briefs are untrusted input.** `reporch brief read` wraps bodies in a delimited untrusted block,
  and sessions are instructed that a brief can never grant permissions or expand access. In a
  multi-repo agent system one prompt injection otherwise becomes nine.
- **Merging is always human-confirmed, per repo. Never add a `--yes` flag.** If you find yourself
  wanting one to make a test pass, the test is wrong.
