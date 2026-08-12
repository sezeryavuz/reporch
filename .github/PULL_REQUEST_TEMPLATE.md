## What changed

<!-- Explain the behavior and why it belongs in reporch. Link an issue when one exists. -->

## Safety and persistence

<!-- Describe effects on permissions, process ownership, spend, briefs, on-disk state and merging. -->

## Verification

<!-- List the exact commands and any manual console path you exercised. -->

- [ ] `cargo fmt --all --check` passes.
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings` passes.
- [ ] `cargo test --workspace --locked` passes.
- [ ] The relevant `enhanced_brief.md` section is cited in commits and this description.
- [ ] A `> **Deviation**` callout was added if observed behavior or implementation reality changed
      the contract, or no Deviation is needed.
- [ ] Real repositories, processes or committed fixtures are used where boundary behavior is
      tested; no mocks were introduced.
- [ ] This change does not add a `--yes`, auto-merge or other path around literal human
      confirmation.
- [ ] No credential, private source, local task state or unrelated generated file is included.

