# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- No changes beyond the prepared 0.1.0 source tree.

## [0.1.0]

Prepared but not yet published.

### Added

- Terminal project creation with repository discovery, relevance ranking, explicit
  `bypassPermissions` consent and Claude-assisted dependency analysis.
- Multi-repository task orchestration with one bounded Claude Code session per worktree, a
  governed brief/ping protocol, live activity and spend views, pause/resume controls, and durable
  local history.
- Structured completion reports and dependency-ordered, per-repository human-confirmed merges
  with resumable checkpoints and worktree teardown.
- Actionable environment diagnostics, update discovery, `reporch` and `rp` binaries, and an
  authored but unexecuted cargo-dist release pipeline.
- Real Git/process tests, recorded Claude stream fixtures, property tests, local package
  verification, cross-platform CI and cargo-deny supply-chain policy.

### Security

- Removed direct session-to-session messaging tools, disabled inherited MCP servers, isolated
  task scratch paths, treated briefs as untrusted input, denied unattended push/PR creation and
  kept merging behind a literal human `y` confirmation.

[Unreleased]: https://github.com/sezeryavuz/reporch/commits/main
[0.1.0]: https://github.com/sezeryavuz/reporch/releases/tag/v0.1.0
