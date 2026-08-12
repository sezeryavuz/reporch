# Phase 0 — spike results

Ten open questions from `enhanced_brief.md` §15, each settled by running a real experiment
against **Claude Code 2.1.226** on macOS 25.0.0 (arm64) on 2026-08-07, then revalidated against
**Claude Code 2.1.228** on the same platform on 2026-08-12.

Every capture referenced here is committed under `fixtures/`. Where a spike returned anything
other than CONFIRMED, a `> **Deviation**` callout has been written into `enhanced_brief.md`.

> **Fixture privacy:** captures retain event ordering, nesting, tool allow/deny evidence, token
> and cost measurements, and terminal outcomes. Machine paths, session/message identifiers,
> hook output, and operator-specific plugin, skill, agent and MCP inventories are replaced with
> deterministic fixture values before commit. The summaries below preserve the measured original
> inventory counts where those counts are part of the finding.

| # | Question | Verdict |
| ---: | :--- | :--- |
| 1 | `stream-json` event schema | **CONFIRMED** (schema captured; three surprises) |
| 2 | stdin injection wakes an idle `-p` session | **CONFIRMED** (0.40s after 45s idle, 0.46s after 180s) |
| 3 | slash commands over stream-json stdin | **CONFIRMED** |
| 4 | `--session-id` survives `--resume` after a kill | **CONFIRMED** |
| 5 | `--max-budget-usd` hard stop | **PARTIAL** — hard stop, but overshoots and kills the process |
| 6 | `claude agents --json` shape; lists `-p` sessions | **PARTIAL** — lists them, but omits `status` |
| 7 | `peerProtocol: 1` socket wire format | **DEFERRED** (v1.1) — sockets observed, protocol not reversed |
| 8 | deny rules remove `SendMessage`/`ListAgents` | **CONFIRMED** with a control |
| 9 | repo-scan wall time | **REFUTED (favourably)** — 0.27s, not the feared 3s+ |
| 10 | `--append-system-prompt` survives compaction | **CONFIRMED** |

## 2026-08-12 revalidation against Claude Code 2.1.228

All ten questions were rerun or deliberately re-evaluated before the open-source hardening pass
was closed. Nine budget-capped Haiku processes covered the paid and zero-cost command surface:
one six-turn process, same-session resumes, deny-rule and invalid-effort controls, a budget-stop
process, and the exact max-plus-ultracode flag combination. Their observed cost totalled
**$0.253**; three slash-only controls cost zero. Harness files, task temporary files, and captures
stayed under the repository's ignored `scratch/` directory, every process exited or was killed by
the harness, and a final `claude agents --json` query returned no `reporch-spike-21228-*` entries.

| # | 2.1.228 evidence | Verdict |
| ---: | :--- | :--- |
| 1 | Known event types and nesting remained parseable. `compact_boundary` gained preserved-segment metadata, which the defensive parser safely ignores. A sanitized current capture is committed as `fixtures/stream/spike10-compact-2.1.228.jsonl` and replayed by a test. | **CONFIRMED** |
| 2 | After 45 seconds idle, the injected message replay arrived in **1.272s** and the assistant answered in **14.455s**. The answer misunderstood the wording, but the transport wake and acknowledgement are unambiguous. A separate five-second check returned the requested marker. | **CONFIRMED** |
| 3 | `/effort high`, `/model`, and `/compact` all returned successfully over stdin. `/model` now advertised `fable`, `best`, `sonnet[1m]`, `opus[1m]`, `fable[1m]`, `opusplan`, and `default` in addition to the familiar aliases. `--effort ultracode` remained accepted while a bogus value and launch-time `auto` were rejected; current docs still define the independent ultracode flag setting reporch uses. | **CONFIRMED** |
| 4 | After `SIGKILL`, `--resume` recalled `PERIWINKLE-42` and returned the same session id. New finding: `total_cost_usd` restarted for the new process rather than including the pre-crash process. See the §10 Deviation and D036. | **CONFIRMED**, with a new accounting constraint |
| 5 | A `$0.05` process made seven sequential Bash calls, emitted `error_max_budget_usd`, exited 1, and reported **$0.0503193**. The hard-stop and between-iterations overshoot remain; the earlier 3.7× overshoot is possible behavior, not a fixed ratio. | **PARTIAL**, unchanged |
| 6 | The live child was still listed as `kind: "interactive"`, with PID/cwd/session id/name and no `status`. | **PARTIAL**, unchanged |
| 7 | Still deliberately deferred: stdin remains conclusive and the public agent listing still exposes no socket path or wire format. No v0.1 path depends on it. | **DEFERRED** |
| 8 | Control: 31 tools, both peer tools present. Deny arm: 29 tools, both absent. Exactly the two named tools were removed. | **CONFIRMED** |
| 9 | The release-built Rust scanner found 162 repositories in **0.235s cold-ish** and **0.046s warm**, better than both the feared threshold and the Python proxy. | **REFUTED favourably**, unchanged |
| 10 | Manual compaction reduced 33,610 tokens to 6,459; the next answer was exactly `REPORCH-REVALIDATION-21228 WORKER`. | **CONFIRMED** |

A parallel documentation sweep (`claude-code-guide` agent, reading `code.claude.com/docs`) ran
alongside these experiments; where documented behaviour and observed behaviour agree, both are
cited. Where the docs said something the experiments did not cover, it is labelled DOCUMENTED-ONLY
and treated as unverified.

---

## Spike 1 — the `stream-json` event schema

**CONFIRMED.** Everything §10 and §13.3 depend on is present and parseable.

**Command** (run by a transient harness under ignored `scratch/`; the resulting capture and
metadata are committed):

```bash
claude -p --input-format stream-json --output-format stream-json --verbose \
  --include-partial-messages --replay-user-messages \
  --model sonnet --permission-mode bypassPermissions --max-budget-usd 0.50 \
  --session-id <uuid> --name reporch-spike-... \
  --settings '{"crossSessionInbound":"accept","permissions":{"deny":["SendMessage","ListAgents"]}}' \
  --append-system-prompt '...' --add-dir <mailbox>
```

stdin, one NDJSON object per turn:

```json
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"..."}]}}
```

**Fixture:** `fixtures/stream/spike01-two-turn.jsonl` (92 events, 2 turns, 3 tool calls).

### Event surface actually observed, across all captures

| `type` (+`subtype`) | Count | What it carries |
| :--- | ---: | :--- |
| `system/init` | 15 | `session_id`, `cwd`, `model`, `permissionMode`, `tools[]`, `skills[]`, `slash_commands[]`, `mcp_servers[]`, `agents[]`, `plugins[]`, `claude_code_version` |
| `system/status` | 8 | `status` (e.g. `requesting`) — coarse in-turn progress |
| `system/compact_boundary` | 1 | `compact_metadata.{trigger,pre_tokens,post_tokens,cumulative_dropped_tokens,duration_ms}` |
| `system/hook_started` / `hook_response` / `hook_progress` | 58 | the *user's own* hooks firing inside our spawned session |
| `system/thinking_tokens` | 63 | thinking budget accounting |
| `system/task_started` / `task_notification` | 2 | subagent lifecycle |
| `assistant` | 36 | `message.content[]` with `thinking` / `text` / `tool_use` blocks; `message.usage` |
| `user` | 20 | **either** a `tool_result` block **or** the injected-turn replay (`isReplay: true`) |
| `stream_event` | 76 | token deltas; only with `--include-partial-messages` |
| `result/success` | 14 | turn boundary |
| `result/error_max_budget_usd` | 1 | budget terminal |
| `rate_limit_event` | 4 | `rate_limit_info.{status,resetsAt,rateLimitType,overageStatus}` |

### Three findings that change the parser

**1. `system/init` is emitted once per _turn_, not once per process.** Two turns produced two
`init` events. A parser that treats `init` as a startup handshake will mis-handle every
subsequent turn.

**2. `result.total_cost_usd` is CUMULATIVE for the session, not per-turn.** Verified across both
captures — the value increases monotonically and per-turn cost is the *delta*:

```
sonnet:  turn1 total=0.384640 (delta 0.384640, 4 iterations, 3 tool calls)
         turn2 total=0.402591 (delta 0.017951, 1 iteration)
haiku:   0.068076 → 0.073920 → 0.084472 → 0.096483 → 0.120690 → 0.124566 → 0.128515
```

The consequence is the opposite of what it first looks like: session **startup** dominates cost
(~$0.38 on Sonnet, ~$0.07 on Haiku, driven by a ~30–55k-token system prompt being cache-written
once), and subsequent turns are cheap (~$0.02). Nine sessions therefore cost roughly $3.50 to
*start* on Sonnet, which makes §8.1's `$10/task` default tight but workable.

**3. Tool results and ping acknowledgements share the `user` type.** The discriminator is
`isReplay: true` on the top-level object — present only on messages echoed back by
`--replay-user-messages`. This is exactly the delivery acknowledgement §6.4 wanted, and it needs
no content-block inspection.

Also: the spawned session inherited the operator's global configuration — 58–60 tools, 178 slash
commands, 116 skills, 30 agents, 14 MCP servers, and their hooks fired inside our session. See
Spike 1b.

### Spike 1b — how much of that surface can be shed (added, not in §15)

One trivial turn on Haiku per configuration (`scratch/probe2.py`):

| Config | tools | slash cmds | skills | MCP | total prompt tokens |
| :--- | ---: | ---: | ---: | ---: | ---: |
| control | 60 | 178 | 116 | 14 | 33,629 |
| `--settings` deny rules | 58 | 178 | 116 | 14 | 33,368 |
| `--strict-mcp-config --disable-slash-commands` | 28 | 0 | 0 | 0 | 29,009 |

Stripping *everything* optional saves ~14%. The remainder is the base system prompt plus built-in
tool definitions and is irreducible. Conclusion: the lean flags are worth taking for **safety**
(no MCP servers in nine unattended bypassing sessions) but they are not a cost lever.

---

## Spike 2 — does stdin injection wake an idle `-p` session?

**CONFIRMED, decisively.** This is the single assumption the whole ping plane rests on.

| Idle before injection | First event | Turn complete |
| ---: | ---: | ---: |
| 0s (first turn) | 1.03s | 20.67s |
| **45s** | **0.40s** | 2.26s |
| **180s** | **0.46s** | 2.25s |

Context survived the gap: after 45s idle the session correctly answered `ORANGE` for a file it had
written before the gap. After the 180s gap it correctly recited its appended-system-prompt marker.

`--replay-user-messages` is a sufficient delivery acknowledgement: every injected turn came back
on stdout as `{"type":"user", ..., "isReplay": true}` before the assistant began responding.

**The process stays alive across turns and exits 0 on stdin EOF.** Confirmed by the harness
driving 7 sequential turns through one child (`fixtures/stream/spike10-compact.jsonl`).

No fallback to the peer socket is needed. §6.4's stdin transport stands.

---

## Spike 3 — slash commands over stream-json stdin

**CONFIRMED.** Sent as ordinary user-message text.

```
→ /effort high
← "Set effort level to high (this session only): Comprehensive implementation..."
→ /model
← "Current model: Haiku 4.5 (effort: high)  Usage: /model <name>. Available: sonnet, opus, ..."
→ /compact
← system/compact_boundary  {trigger: "manual", pre_tokens: 35833, post_tokens: 7955}
```

Effort and model are therefore changeable **mid-task**, not only at spawn. reporch does not need
this for v1 but it removes a constraint from §5.6, and `/compact` gives a manual pressure valve
for a session approaching its context limit.

---

## Spike 4 — `--session-id` + `--resume` after a crash

**CONFIRMED.** Procedure: establish a codeword, `SIGKILL` the child (`exit -9`), respawn with
`--resume <same-uuid>`, ask for the codeword.

```
killed_exit: -9
spike4_recalled: "PERIWINKLE-42"
spike4_same_session_id: true
```

Full context restored, same session id.

The 2.1.228 revalidation found one additional constraint: `result.total_cost_usd` restarted at
`$0.0199138` in the resumed process after the original process had reached `$0.1035003`, despite
the unchanged session id and restored transcript. "Cumulative for the session" is therefore too
broad: the field is cumulative for one live process epoch. A future respawn control must persist
the old epoch's final spend and add the new epoch rather than replacing it.

> **DOCUMENTED-ONLY caveat, not tested:** the docs state that launch flags are *not* restored on
> resume — `--mcp-config`, `--settings`, `--add-dir`, `--plugin-dir`, `--fallback-model` must all
> be passed again. `--append-system-prompt` is not named either way. reporch therefore re-passes
> its **entire** flag set on every respawn and never assumes a resumed session inherited anything.

---

## Spike 5 — is `--max-budget-usd` a hard stop?

**PARTIAL.** It is a hard stop, it overshoots, and it kills the process.

Command: a turn requiring ~10 sequential Bash calls, with `--max-budget-usd 0.05`.

```
subtype:         error_max_budget_usd
terminal_reason: budget_exhausted
is_error:        true
stop_reason:     tool_use
total_cost_usd:  0.1862916        ← 3.7× the $0.05 ceiling
process exit:    1
```

On 2.1.228 the same shape of probe stopped after seven Bash iterations at `$0.0503193`. This does
not retract the original result: enforcement still happens between iterations and can overshoot;
the magnitude depends on the cost of the iteration that crosses the threshold.

Three consequences for §10 and §12.3:

1. **The ceiling is enforced between iterations, so it overshoots by up to one iteration.** At
   small ceilings that ratio is large; at realistic ceilings ($5–10) the overshoot is a rounding
   error. Never quote `--max-budget-usd` to a user as an exact cap.
2. **Hitting the ceiling terminates the process** — the session does not pause and wait. reporch
   must treat `terminal_reason: budget_exhausted` as session death: mark the repo `dead`, surface
   it, and offer a resume with a raised ceiling (documented as the supported recovery).
3. `total_cost_usd` being cumulative (Spike 1) is what makes the ceiling meaningful at all —
   reporch can track live spend per session straight off the last `result`.

---

## Spike 6 — `claude agents --json`

**PARTIAL.** It lists app-spawned `-p` sessions, but not with the fields §4.1 assumed.

Observed entry for a live reporch-spawned `-p` child:

```json
{ "pid": 26769, "cwd": ".../scratch/spikerepo", "kind": "interactive",
  "startedAt": 1786163651297, "sessionId": "996d96db-...", "name": "reporch-spike-multi" }
```

Compared with the registry file `~/.claude/sessions/26769.json` for the same process:

```json
{ "pid": 26769, "sessionId": "996d96db-...", "cwd": "...", "startedAt": 1786163651297,
  "procStart": "Sat Aug  8 04:34:09 2026", "version": "2.1.226", "peerProtocol": 1,
  "kind": "interactive", "entrypoint": "sdk-cli",
  "messagingSocketPath": "/tmp/cc-socks/26769.sock", "name": "reporch-spike-multi",
  "updatedAt": 1786163651304 }
```

Three findings:

1. **`-p` sessions do not report `status`.** Both `claude agents --json` and the registry file
   omit `status`/`statusUpdatedAt` for them; interactive sessions have both. §4.1's plan to read
   busy/idle liveness from the registry **does not work for reporch's own sessions.**
2. **`kind` is `"interactive"` for a `-p` session** — useless as a discriminator. The registry's
   `entrypoint` field (`"sdk-cli"` vs `"cli"`) is the real one, and it exists only in the file,
   not in `claude agents --json`.
3. `messagingSocketPath` and `version` exist only in the registry file.

None of this is fatal, because it is the wrong source of truth anyway: reporch owns each child's
stdout, so it knows a turn started (first event after injection) and ended (`result`) exactly,
with no polling. Liveness moves from the registry to the stream. See the Deviation in §4.1.

`claude agents --json` remains useful for one thing: **orphan detection** — finding reporch-named
sessions still alive after a crash.

---

## Spike 7 — the peer socket

**DEFERRED to v1.1, as §15 instructs.** Observed only:

```
srw-------@ 1 fixture-user wheel 0 Aug  7 23:34 /tmp/cc-socks/fixture.sock
```

Owner-only Unix domain sockets, one per pid, and **`-p` sessions do bind one**. The registry
advertises it as `messagingSocketPath` and `peerProtocol: 1`. The wire format was not reverse
engineered and nothing in v1 depends on it.

Two documented facts collected for the v1.1 design: the socket path is exported to hooks and Bash
as `CLAUDE_CODE_MESSAGING_SOCKET`, and `--bare` sessions do **not** bind a socket at all.

---

## Spike 8 — do deny rules actually remove the tools?

**CONFIRMED, with a control.** Absence alone would prove nothing, so both arms were run
(`scratch/probe2.py`, one trivial Haiku turn each, reading `system/init.tools`):

| Run | `--settings` | `tools` | `SendMessage` present | `ListAgents` present |
| :--- | :--- | ---: | :--- | :--- |
| control | *(none)* | **60** | **true** | **true** |
| deny | `{"permissions":{"deny":["SendMessage","ListAgents"]}}` | **58** | **false** | **false** |

Exactly two tools removed. The model never sees them. §6.2's first enforcement mechanism holds.

The 2.1.228 control reproduced the exact delta with the lean spawn surface: 31 tools without the
deny payload, 29 with it, and only `SendMessage`/`ListAgents` disappeared.

This also hands reporch a **spawn-time assertion that is better than trusting the flag**: the
`-p` docs warn that "settings files that fail validation are silently ignored in this mode".
reporch therefore reads back the first `system/init` and asserts `SendMessage` and `ListAgents`
are absent from `tools[]`, failing the spawn loudly if they are not. A typo in the settings JSON
cannot silently disable the protocol.

---

## Spike 9 — repo-scan wall time

**REFUTED, favourably.** The spec feared >3s, which would make the §5.3 cache load-bearing.

Real home directory, pruning at `.git` (a repo is a leaf), skip-list applied, in **single-threaded
Python** — a deliberately pessimistic proxy for the parallel, native `ignore` crate:

| max depth | repos found | dirs visited | wall |
| ---: | ---: | ---: | ---: |
| 6 | 149 | 2,814 | 0.367s |
| 8 | 149 | 4,529 | 0.282s |
| 10 | 149 | 6,582 | 0.271s |
| 10 (warm) | 149 | 6,582 | **0.152s** |

An order of magnitude below the threshold before the real implementation is even written. The
cache stays (§5.3 asks for it and it is ~40 lines) but it is an optimisation, not a dependency.
Depth beyond 6 adds no repos on this machine; the default bound of 8 is comfortable.

The number to beat with the real `ignore`-based scanner: **0.27s cold.**

The release-built Rust implementation beat it during 2.1.228 revalidation: 162 repositories in
0.235s cold-ish and 0.046s warm on the same machine.

---

## Spike 10 — does `--append-system-prompt` survive compaction?

**CONFIRMED.** The most structurally dangerous open question, and the answer is the good one.

A marker was placed in the appended system prompt only — nowhere in the conversation — then
compaction was forced with `/compact` (cheaper than burning 100k tokens to trip auto-compaction,
and the docs confirm both paths emit the same boundary event).

| Step | Reply | Marker intact |
| :--- | :--- | :--- |
| baseline, "what is your protocol marker?" | `REPORCH-PROTOCOL-MARKER-7F3A` | ✅ |
| baseline, "what is your role?" | `WORKER` | ✅ |
| `/compact` | `compact_boundary` — 35,833 → 7,955 tokens, 27,878 dropped | — |
| **after compaction**, marker | `REPORCH-PROTOCOL-MARKER-7F3A` | ✅ |
| **after compaction**, role | `WORKER` | ✅ |
| after a further **180s idle**, marker | `REPORCH-PROTOCOL-MARKER-7F3A` | ✅ |

Corroborated by documentation, which states the rule directly: *"System prompt and output style →
Unchanged; not part of message history."*

So §7.3's protocol-injection strategy is sound and needs no re-injection timer. reporch still
parses `system/compact_boundary` — it is a genuine signal for the run screen and for the log — but
nothing depends on reacting to it.

The 2.1.228 capture again returned the marker and role after compaction. Its boundary added
preserved-message UUID metadata, but retained the same `trigger`, `pre_tokens`, and `post_tokens`
fields reporch consumes. The minimized committed capture is replayed in the test suite.

> **Caveat worth recording:** the same documentation table says rules with `paths:` frontmatter and
> *nested* `CLAUDE.md` files are **lost** at compaction until a matching file is read again. Since
> §7.3 already forbids reporch from writing into `CLAUDE.md`, reporch is unaffected — but it is
> another reason the `--append-system-prompt` decision was the right one.

---

## Documented facts collected alongside, that change the design

These came from the documentation sweep rather than an experiment. Each is labelled by how it was
established. All five are written into `enhanced_brief.md` as Deviations or Additions.

**A. `--bare` would silently destroy the ping plane. DOCUMENTED.** A `--bare` session does not
bind an inbox socket, cannot receive messages, and does not appear in the agent list. The docs
call `--bare` "the recommended mode for scripted and SDK calls" and say it "will become the
default for `-p` in a future release". reporch must never spawn with `--bare`, and `reporch
doctor` should assert the spawned session still binds a socket so a future default flip is caught
loudly rather than silently.

**B. `crossSessionInbound` has inverted precedence. DOCUMENTED.** Unlike every other key, a
*stricter* value in a repo's own `.claude/settings.json` overrides the `--settings` value reporch
passes: the ladder is `accept < hold < refuse` and project/local settings apply "only when
stricter". A repo containing `"crossSessionInbound": "refuse"` cannot be forced to `accept`. The
docs add that a refusing session shows no visible change anywhere. §4.4's "assert it at spawn" is
therefore *insufficient* — but see the Deviation in §4.4: because reporch pings over **stdin**,
not over cross-session messaging, this affects only third-party messages into reporch's sessions.
It is a warning, not a blocker.

**C. `--add-dir` loads skills and subagents from the target directory's `.claude/`. DOCUMENTED.**
Only `enabledPlugins`/`extraKnownMarketplaces` are read from its `settings.json`, and `CLAUDE.md`
needs an env var — but skills and agents load, with live reload. reporch's `--add-dir` target is
the task mailbox, a reporch-owned directory with no `.claude/`, so the exposure is nil today. It
is recorded because it constrains what `--add-dir` may ever point at.

**D. `ultracode` *is* an accepted `--effort` value. OBSERVED + DOCUMENTED.** `claude --help` lists
only five levels, and an invalid value produces `Warning: Unknown --effort value 'bogusvalue' —
ignoring it... Valid values: low, medium, high, xhigh, max`. `--effort ultracode` produces **no
such warning**, exactly like `xhigh` and `max`. Documentation confirms it: it starts the session
at `xhigh` effort with ultracode on. §4.3's Deviation is therefore half wrong and is corrected —
though the *product* design in §5.6 is unaffected, and reporch still uses `--effort max` plus the
`ultracode` settings key so that "max + ultracode" is expressible at all.

**E. Four excluded providers, not three. DOCUMENTED.** Amazon Bedrock, **Claude Platform on AWS**,
Google Cloud's Agent Platform, and Microsoft Foundry. `reporch doctor` checks all four.

---

## Cost of Phase 0

Roughly **$1.60** across nine spawned sessions, all budget-capped, all terminated. Verified zero
orphans at the end of the phase:

```
$ claude agents --json | ... | grep -c 'reporch-spike\|reporch-probe'
0
```

The 2.1.228 revalidation added approximately **$0.253** across nine spawned processes, three of
which were zero-cost slash-command controls. The final filtered agent listing was again `[]`.
