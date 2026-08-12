# Troubleshooting

Start with the environment report:

```bash
reporch doctor
reporch doctor --json
```

The text form gives remedies; the JSON form is easier to attach to a bug report after redacting
paths and provider details. The console runs the same checks before opening and refuses to start
on a fatal finding.

## Where to look first

| Symptom | First evidence |
| :--- | :--- |
| Console will not open | `reporch doctor`; `~/.reporch/logs/reporch.log` |
| One repository looks stuck | Select it and press `enter`; press `b` for the mailbox |
| Delegation did not arrive | Mailbox timeline, then `briefs/index.json` |
| Session exited or hit budget | The repository's `logs/<repo>.jsonl` and `task.json` |
| Merge is blocked | The current diff, test output and `merge.json` |
| A crashed reporch may have left children | `claude agents --json`, filtered by the exact task alias |

Never post a complete task directory publicly. It can contain source excerpts, prompts, model
transcripts and carried configuration. Redact before sharing diagnostics.

## A session says `reporch: command not found`

Claude Code's Bash tool can start a login shell that rebuilds `PATH`. reporch therefore injects
the absolute executable path as `REPORCH_BIN`, and the protocol tells sessions to use it:

```bash
"$REPORCH_BIN" brief inbox
```

If that variable is missing or points nowhere, the binary was probably moved or replaced after
the task started, or the session was not started by reporch.

1. In the operator terminal, verify the binary still exists and runs: `reporch doctor`.
2. Do not move or overwrite the executable while a task is active.
3. Preserve the task's worktrees, abort the broken run through the console, and start a new task
   from the fixed binary.

Do not tell a session to search arbitrary filesystem locations for the executable. Its refusal to
act on an unverifiable brief is the safe behavior.

## A repository is `dead` after budget exhaustion

Claude Code checks `--max-budget-usd` between model iterations. It can overshoot the displayed
ceiling by one iteration and then exits with `terminal_reason: budget_exhausted`; it does not pause
in place. reporch records the last cumulative cost, marks the repository dead and keeps the
worktree and raw stream.

The current alpha does not expose a safe in-console respawn with a raised ceiling. Inspect the
worktree and session log, then either finish the remaining work manually or abort and create a
new, deliberately budgeted task. Never loop an automatic resume around a budget failure—the
ceiling is the brake that stopped the spend.

## The task reached its wall-clock ceiling

A delayed task has already stopped its sessions; its worktrees are intentionally preserved.
Inspect `task.json`, the mailbox and each repository's last stream events. Copy or commit valuable
work before deciding whether to continue manually or start a replacement task. Removing the task
record does not itself clean Git worktree registrations.

## A stale worktree blocks task creation

reporch automatically prunes a stale Git registration when its recorded path no longer exists.
It will not remove a real worktree that is still on disk, because it may contain uncommitted work.

For the repository named in the error:

```bash
git -C /path/to/source-repo worktree list
git -C /exact/reporch/worktree/path status --short
```

Inspect the exact listed worktree and preserve any changes. If it is unquestionably an abandoned
reporch task worktree, remove that exact path with Git and then prune:

```bash
git -C /path/to/source-repo worktree remove /exact/reporch/worktree/path
git -C /path/to/source-repo worktree prune
```

Do not recursively delete a project, source repository or broad `~/.reporch/worktrees` root to
solve one registration. If Git refuses removal because the worktree is dirty, that is evidence to
review, not a flag to force blindly.

## Analysis warns that a repository refuses inbound messages

A checked-in or local `.claude/settings.json` may set `crossSessionInbound` to `hold` or `refuse`.
Claude Code gives the stricter repository value precedence over reporch's injected `accept`, so
reporch cannot override it.

This warning does not break reporch's own ping plane: reporch wakes its children over owned stdin,
and direct Claude messaging tools are denied. It does mean third-party sessions cannot message
that child as expected. If the repository owner intentionally changes the setting, re-run project
analysis so the warning reflects the new file. Do not rewrite repository policy automatically.

## A brief exists but the recipient did not act

Content and attention are separate. A valid brief file does not prove a wake-up was authorized or
delivered.

1. Press `b` on the run screen and locate the brief and any following ping entry in chronological
   order.
2. Check whether the recipient is idle, working, dead or already has unacknowledged work.
3. Open `briefs/index.json` to inspect `read_at`, status and ping records.
4. Open the recipient stream with `enter` and look for the replayed user message that acknowledges
   stdin delivery.
5. If a worker needs another worker, verify it briefed the master for attention; workers cannot
   ping peers directly.

The supervisor rate-limits duplicate attention and drops identical consecutive ping bodies. A
delay can therefore be deliberate. Do not bypass the protocol with Claude's direct messaging
tools.

## Project analysis failed or produced an implausible graph

Analysis uses structured Claude output for every selected repository, then synthesizes the
project. A non-JSON answer, unknown repository name or missing graph field fails visibly rather
than being guessed.

- Read the progress/error screen and `~/.reporch/logs/reporch.log`.
- Verify each selected path still exists and is a readable Git repository.
- Run `reporch doctor` before paying for another attempt.
- From project settings, re-run analysis. Existing budget, carry-over settings and the next task
  ID are preserved while repository facts and the graph are replaced.

Read `project_base.md` and the files under `repos/` before trusting an unusual master election or
merge order. Those readable files are the diagnostic surface for the model's conclusions.

## A merge step failed

Rebase conflicts, repository hooks, configured tests, a changed source checkout or missing Git
identity can all block a confirmed step. reporch records the failure and preserves the worktree.
It never converts the error into a skip and never spends the old `y` confirmation twice.

1. Read the exact merge-screen error and current diff.
2. Repair the named worktree or source checkout with ordinary Git tools.
3. Return to the completed task. Press `y` only after reviewing the repaired diff, or press `s`
   and then `y` to explicitly skip that repository.
4. If Git already succeeded but checkpoint persistence or teardown failed, use the screen's `r`
   recovery. That path verifies/checkpoints state and does not replay the merge.

`merge.json` is readable evidence of every terminal step and recorded commit. On reopen, reporch
accepts a recorded merge only if Git still proves that commit is an ancestor of the source branch.

## The console display is damaged

`Ctrl-C` and ordinary errors restore raw mode and leave the alternate screen. If a hard external
kill leaves the terminal in an unusual state, run your shell's `reset` command. Developers can
exercise the panic restoration hook deliberately:

```bash
cargo run -p reporch -- panic-test
```

The command is supposed to panic; the terminal must still accept normal input afterward.

## Inspecting logs without corrupting the console

Application tracing goes only to:

```text
~/.reporch/logs/reporch.log
```

Set `REPORCH_LOG` before launch to change the tracing filter. Per-session raw streams live under
the task's `logs/` directory and are also available from the run screen with `enter` or `l`.
stdout and stderr are intentionally not logging destinations because one is the TUI and the other
may be machine-readable JSON.

Merged tasks are compacted: raw session logs and worktrees are removed after merge, while the
brief, structured result, merge checkpoint and compact task record remain.

## Checking for orphaned Claude sessions

After a reporch crash, inspect Claude's public registry output:

```bash
claude agents --json | jq '[.[] | select((.name // "") | contains("@TASK-"))]'
```

The `kind` and `status` fields do not reliably identify app-owned `-p` sessions. Match the exact
`<repo>@<TASK-ID>` name and confirm its `cwd`, PID and task before acting. Other interactive Claude
sessions belong to the user and must be left alone.

If an entry is certainly an orphan from the crashed task, first send a normal termination signal
to that exact PID and check the list again. Use a forced kill only if the validated process does
not exit. Then inspect and preserve its worktree; stopping the process does not merge or delete
the repository changes it made.

## Reporting a bug

Use the repository's bug form, which requires `reporch doctor` output and `claude --version`.
Include the smallest redacted task-state or stream excerpt that proves the behavior. Security
findings go through the private process in [`SECURITY.md`](../SECURITY.md), never a public issue.
