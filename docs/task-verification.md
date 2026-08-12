# Console task verification

This records the paid terminal runs used to verify the complete console path on 2026-08-12 with
Claude Code 2.1.228. Both used the repository-local `codex-analysis-proof` project documented in
[`analysis-verification.md`](analysis-verification.md), Haiku, low effort, and a `$0.25`
per-session ceiling (`$0.75` worst-case task ceiling).

## Three-session completion and live controls

The first task asked the elected `contracts-api` master to inspect consistency without changing
code. From the release console alone it:

- created three worktrees and three Claude processes;
- entered the live run screen with real state, activity, brief counts, and cost columns;
- opened `b` mailbox, `l` all logs, and `enter` persisted session output while work continued;
- paused and resumed attention with `p`, with both transitions visible in notices;
- had the master file two request briefs despite the narrow wording of the task;
- delivered the two worker pings through the control plane;
- had both workers read, complete, and acknowledge their briefs;
- wrote schema-valid `result.json` / `result.md`; and
- reached `completed` on its own after 1m30s.

The report correctly listed all three repositories as unchanged. The release console then opened
the task from **completed tasks**, displayed the real merge review, recorded all three outcomes as
`unchanged`, ran idempotent teardown, wrote `merge.json`, moved the task to `merged`, removed this
task's worktrees and branches, and compacted its history.

This run exposed an accounting defect: the supervisor treated `finish-requested` as terminal even
though the command executes inside the master's still-open Claude turn. It closed stdin before
that turn's `result`, leaving the master persisted as `working` at `$0.00`; the recorded
`$0.1487777` was therefore only the sum of the two workers and not the real task total. The raw
master log ended mid-stream with no result event. D043 records the fix: transport-level open turns
are now tracked independently, and completion waits for the master's result telemetry.

The same run also exposed that a completed run screen could not return to the project even though
its notice instructed the user to do so. The finished footer and `Esc`/`q` navigation now return
after the task actor has stopped, with **completed tasks** highlighted.

## Post-fix master-only proof

The second task was deliberately narrow: inspect only `contracts-api/schema.json`, make no changes,
file no briefs, and submit a structured unchanged result for every repository.

The model picker opened with `haiku` already selected from `ProjectSettings.default_model`, proving
the configured-default fix. The task completed after 22 seconds. The two unbriefed worker
processes never took a turn and stayed at zero cost, confirming the paid-startup behavior behind
D039. Before merge compaction, the master log ended with a real `result/success`; `task.json`
matched it exactly:

```text
task status                 completed
model / effort              haiku / low
task spend                  $0.0633159

contracts-api state         idle
contracts-api spend         $0.0633159
input tokens                41
output tokens               1,419
cache-read tokens           164,419
cache-creation tokens       19,869

contracts-client            spawning · $0.00 · no token usage
contracts-docs              spawning · $0.00 · no token usage
```

`Esc` then returned directly from the stopped completed run to the project menu. Opening the task
showed all three repositories as unchanged; `r` persisted all decisions, removed the task's
worktrees and branches, compacted logs, and changed the task to `merged`.

After each run, the three source checkouts retained their pre-existing status and this
workspace-scoped orphan query returned `[]`:

```bash
claude agents --json \
  | jq --arg root "$PWD/scratch" '[.[] | select(.cwd | startswith($root))]'
```

These tasks intentionally made no product diff. A separate three-repository paid proof produced
coherent real changes and was merged with literal per-repository `y` confirmations; its ignored
artifacts remain under `scratch/codex-e2e-completion-20260812/`.
