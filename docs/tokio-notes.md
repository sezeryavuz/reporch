# tokio notes — reporch's exact shape

Distilled from **tokio 1.53.1**, read from the vendored crate source at
`~/.cargo/registry/src/*/tokio-1.53.1/src/process/mod.rs`. Written before the session supervisor,
so the traps are written down before they are hit rather than after.

reporch's shape is narrow and specific:

> **Spawn nine children with piped stdio. Write JSON lines to each one's stdin while concurrently
> reading JSON lines from its stdout. Shut all of them down cleanly without orphaning any.**

That last clause is not theoretical. **An orphaned Claude session keeps spending money.**

---

## 1. The shape

```rust
let mut cmd = tokio::process::Command::new("claude");
cmd.args(&argv)
   .current_dir(&worktree)
   .envs(&env)
   .stdin(Stdio::piped())
   .stdout(Stdio::piped())
   .stderr(Stdio::piped())
   .kill_on_drop(true);            // see §3 — this is the money one

let mut child = cmd.spawn()?;

// Take the handles out. Partially moving `child` would block calling `wait()` on it later —
// the source comments say so explicitly.
let stdin  = child.stdin.take().expect("piped");
let stdout = child.stdout.take().expect("piped");
let stderr = child.stderr.take().expect("piped");
```

**One task per direction, not one task doing both.** Reading and writing must not be sequenced
against each other: a session mid-turn produces a continuous stream of events, and a supervisor
that is blocked writing an injection is a supervisor not reading. They are independent, so they
are separate tasks joined by channels.

```
   injections ──► [writer task] ──► child stdin
                                    child stdout ──► [reader task] ──► events channel ──► supervisor
```

---

## 2. Reading lines

```rust
use tokio::io::{AsyncBufReadExt, BufReader};

let mut lines = BufReader::new(stdout).lines();
while let Some(line) = lines.next_line().await? {
    // parse and forward
}
// next_line() -> Ok(None) means EOF: the child closed stdout, i.e. it is exiting.
```

`BufReader` matters. Claude Code with `--include-partial-messages` emits a token-level event per
delta — the Phase 0 fixture has 76 `stream_event` lines for two short turns — and an unbuffered
read syscall per line is wasteful at that rate.

**`next_line()` strips the newline and handles `\r\n`.** Do not re-trim and do not assume a line
is non-empty; the parser already returns `None` for blank lines.

---

## 3. The traps

### Dropping the `Child` handle does **not** kill the process

Straight from the source:

> *"Similar to the behavior to the standard library, and unlike the futures paradigm of
> dropping-implies-cancellation, a spawned process will, by default, continue to execute even
> after the `Child` handle has been dropped."*

For reporch this is the difference between a clean abort and nine `claude` processes still
burning budget with nobody watching. So:

- **`kill_on_drop(true)` on every spawn.** It is the backstop for a panic or an early return.
- **And still call `kill().await` explicitly** on teardown. `kill_on_drop` fires only if the
  runtime is alive to reap; a hard `abort()` or a `SIGKILL` of reporch itself will not run it.
- The last line of defence is outside the process entirely: `claude agents --json` filtered by the
  `<repo>@<TASK_ID>` alias convention finds orphans a crashed reporch left behind
  (`reporch-claude::registry`).

### Never hold a `std::sync::Mutex` across `.await`

CLAUDE.md states this and it applies directly here: the supervisor owns `TaskState`, and the
temptation is to lock it, then `await` a write to a child's stdin while holding the lock. That
deadlocks the moment two sessions do it in opposite orders, and it is not a deadlock that shows up
in testing.

reporch avoids the question entirely: `TaskState` lives in **one** task and is never shared.
Everything else talks to it over channels. The pure `(TaskState, Event) -> (TaskState, Vec<Action>)`
signature is what makes that possible — the state never needs to be reachable from anywhere else.

Where a lock genuinely is needed (the mailbox index, shared with other *processes*), it is an
advisory file lock in synchronous code, taken and released without an `.await` in between.

### Blocking the runtime

`std::fs`, `std::process::Command::output()`, and `git` invocations are all blocking. The
supervisor's tick is 250ms; a synchronous `git worktree add` inside it stalls every session's event
pump. Blocking work goes through `tokio::task::spawn_blocking`, or happens before the runtime is
carrying live sessions.

### Zombies surviving a crash of reporch itself

`kill_on_drop` cannot help here — reporch is gone. Two mitigations, both cheap:

1. `task.json` records `session_pid` and `session_id` at spawn, so a later run knows what to look
   for.
2. On startup, reporch reconciles: any live session whose alias matches a task that is not running
   is an orphan, and is reported before anything else happens.

### `wait()` must be awaited by someone

A child that exits and is never `wait()`ed becomes a zombie in the process table. The supervisor
owns exactly one `wait()` per child and treats its completion as the authoritative death signal —
not stdout EOF, which can happen first.

---

## 4. Shutdown that does not orphan

Order matters, and it is the reverse of what feels natural:

1. **Close stdin.** Dropping `ChildStdin` sends EOF, and a `-p` session exits cleanly on stdin EOF
   (verified in spike #2). This is the graceful path and it lets the session finish its turn.
2. **Wait, with a timeout.** `tokio::time::timeout(GRACE, child.wait())`.
3. **`start_kill()` / `kill().await`** if the grace period expires.
4. **`wait().await`** again to reap.

`start_kill()` is the non-async variant — useful in a `Drop` impl or an abort path where there is
nothing to await on.

```rust
drop(stdin);                                            // EOF: ask it to finish
match tokio::time::timeout(GRACE, child.wait()).await {
    Ok(Ok(status)) => status,                           // exited on its own
    _ => { let _ = child.start_kill(); child.wait().await? }
}
```

---

## 5. Cancellation

`tokio::select!` cancels the losing branch by dropping its future. That is safe for
`next_line()` (it is cancel-safe) but **not** for a partially completed write. reporch writes a
whole injection line in one `write_all` and only then awaits anything else, so a cancelled writer
never leaves half a JSON line in a child's stdin — which would desynchronise the session's parser
permanently.

For fan-out over nine sessions, `JoinSet` gives per-task completion as it happens rather than
waiting for all of them:

```rust
let mut set = JoinSet::new();
for spec in specs { set.spawn(async move { create_worktree(spec).await }); }
while let Some(res) = set.join_next().await {
    // handle each as it lands; a panic surfaces as Err(JoinError)
}
```

`JoinSet` aborts every remaining task when dropped, which is the behaviour reporch wants on abort.

---

## 6. Testing without spending money

The supervisor is written against a **trait** rather than `tokio::process::Child` directly, so the
whole loop can be driven by replaying `fixtures/stream/spike01-two-turn.jsonl` — a real captured
session — at zero cost and in milliseconds. The stream-json parser is already tested against that
fixture; this extends the same fixture to the supervisor.

The first time real money is spent should be the *last* step, not the first.
