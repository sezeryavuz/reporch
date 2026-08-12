# Contributing to reporch

Thank you for helping improve reporch. This project coordinates multiple unattended Claude Code
sessions with broad local permissions, so changes are expected to preserve its unusually strict
human-confirmation and auditability boundaries.

## Development setup

Install Git and the Rust toolchain selected by `rust-toolchain.toml`, then clone the repository.
Claude Code is not required for ordinary builds or tests.

```bash
cargo build --workspace --locked
cargo test --workspace --locked
```

The main local gate runs formatting, Clippy, tests, rustdoc and a release build:

```bash
cargo xtask verify
```

Before changing dependencies, also install
[`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny) and run:

```bash
cargo deny check
```

`cargo xtask package` verifies all six publishable archives locally. It never publishes them.
Do not run `cargo publish`, the release workflow, or any distributor command while validating a
contribution.

## How this repository is specified

[`enhanced_brief.md`](enhanced_brief.md) is the numbered product contract. Cite the relevant
section in behavior-changing commits, for example:

```text
M6: preserve failed merge worktrees (§11.2)
```

Reality wins over the written design. If observed Claude Code, Git or platform behavior
contradicts the specification, add a `> **Deviation**` callout to `enhanced_brief.md` explaining
what is true and what changes as a consequence. Record consequential forks in
[`docs/decisions.md`](docs/decisions.md): the options, the chosen path, what was rejected and why.

Two invariants are non-negotiable:

- A repository merge is authorized only by a literal `y` from the human merge screen. Never add a
  `--yes`, auto-merge or unattended confirmation path.
- A worker can never ping another session. Briefs carry content; the reporch control plane carries
  rate-limited attention according to §6.

## Testing standard

Tests assert behavior at the boundary where it matters. The project deliberately has no mocks:

- Git behavior uses temporary, real repositories and real `git` processes.
- Process behavior uses small real child processes.
- Claude stream behavior replays committed captures under `fixtures/stream/`.
- Filesystem behavior uses isolated temporary directories and real atomic writes.

Prefer a committed fixture or a tiny deterministic process over a fake implementation. Tests must
not read or mutate the contributor's real `~/.reporch`, `~/.claude` or repositories. Any test that
would spend Claude API budget belongs in the explicitly paid spike or end-to-end procedure, not in
`cargo test`.

For a focused test, select the owning crate first, then finish with the full gate:

```bash
cargo test -p reporch-core protocol
cargo xtask verify
```

## Rechecking Claude Code behavior

The ten paid experiments and their recorded results live in [`docs/spikes.md`](docs/spikes.md).
Re-run them when Claude Code changes minor version or when a depended-on flag, event or registry
shape changes.

1. Read the old spike and its fixture before running anything. Record `claude --version`, OS,
   architecture and date in the replacement metadata.
2. Reproduce the exact bounded command shown in that spike. Keep each session's
   `--max-budget-usd` small, run one uncertainty at a time, and keep transient harnesses under
   ignored `scratch/`.
3. Save stable NDJSON and metadata under `fixtures/stream/`; redact credentials and private paths,
   but do not simplify or hand-edit the event shape the parser is meant to learn from.
4. Terminate every spawned process, then verify no matching session remains with
   `claude agents --json`.
5. Update `docs/spikes.md`, add any required Deviation, and run `cargo xtask verify` so the new
   fixture is exercised by the parser tests.

Spike runs spend real money and use `bypassPermissions`. Never run them in an unrelated or dirty
repository, and never leave a session unattended after the experiment finishes.

## Pull requests

Keep a change coherent and reviewable. Explain user-visible behavior, safety consequences and the
commands you ran. Do not include generated `target/` content, local state, credentials, model
transcripts that were not intentionally curated as fixtures, or unrelated formatting churn.

By participating, you agree to follow the [Code of Conduct](CODE_OF_CONDUCT.md). Security issues
must be reported privately according to [SECURITY.md](SECURITY.md), not through a public issue.

