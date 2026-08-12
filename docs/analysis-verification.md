# Project-analysis verification

This records the first project analysis exercised through the real `reporch` console against
multiple Git repositories. It is evidence, including the imperfect result, rather than a hand
edited example.

## 2026-08-12 — Claude Code 2.1.228

The release binary ran with a repository-local `REPORCH_HOME` and a scan root containing three
real Git repositories under `scratch/e2e`: `contracts-api`, `contracts-client`, and
`contracts-docs`. They are deliberately small, but they contain an actual shared contract in
Python, JSON, and Markdown. No external repository was read or written.

From the console, the proof performed this exact sequence:

1. selected **new project** and entered `codex-analysis-proof`;
2. observed the scanner find and rank exactly the three repositories;
3. selected all three with `a` and continued;
4. verified that consent displayed three sessions and the configured `$0.25` future task-session
   ceiling, then typed the required literal `y`;
5. observed three independent `reading… Ns` rows update concurrently while the terminal remained
   responsive; and
6. waited for the console to display `done` / `Ready.` before exiting normally.

Analysis used `haiku`. The implementation independently caps each read-only analysis subprocess
at `$1.00` and the synthesis subprocess at `$0.50`; all four processes exited normally. The
following artifacts were written by that console run:

```text
projects/codex-analysis-proof/
├── project_base.json
├── project_base.md
└── repos/
    ├── contracts-api.json
    ├── contracts-client.json
    └── contracts-docs.json
```

The generated overview accurately described one authoritative Widget schema, one programmatic
consumer, and one documentation consumer. All three profiles reported `confidence: high`. The
stored dependency facts were:

```text
contracts-api    consumes: —                  produces for: client, docs
contracts-client consumes: contracts-api      produces for: —
contracts-docs   consumes: —                  produces for: —
```

This is useful and not perfect. The model found both downstream consumers from the producer's
README, and it found the client's reciprocal `consumes` fact, but it did not infer the docs
repository's reciprocal fact from its terse README. `DepGraph` merges `consumes` and
`produces_contracts_for`, so the effective API-to-docs edge is still present for master election
and merge ordering. This demonstrates why the two-sided schema and editable master choice matter;
model confidence is not a guarantee that every redundant field agrees.

Afterward, all three source repositories had the same status they had before analysis, and this
filtered orphan check returned `[]`:

```bash
claude agents --json \
  | jq --arg root "$PWD/scratch" '[.[] | select(.cwd | startswith($root))]'
```

Limitations: these are small repository-local fixtures, not a large private production project;
the proof validates the live console, structured model output, persistence, and graph behavior,
but it does not establish analysis quality for every framework or a large, ambiguous dependency
topology.
