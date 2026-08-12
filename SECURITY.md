# Security policy

reporch deliberately starts multiple Claude Code sessions in `bypassPermissions` mode. That makes
the safety boundary more important than it is for an ordinary terminal application. Please read
this document before enabling a project.

## Supported versions

reporch has not published its first release. Security fixes currently target the latest commit on
`main`; old commits and locally modified builds are not supported.

| Version | Supported |
| :--- | :---: |
| `main` | Yes |
| Any packaged release | None published |

## Report a vulnerability privately

Do not open a public issue for a suspected vulnerability. Prefer GitHub's private vulnerability
reporting form at
<https://github.com/sezeryavuz/reporch/security/advisories/new>. If that form is unavailable,
email [hey@sezeryavuz.com](mailto:hey@sezeryavuz.com) with the subject `[reporch security]`.

Include the affected commit or version, platform, impact, reproduction steps and the smallest safe
proof of concept. Do not attach live credentials, private repository contents, `~/.reporch` task
trees or unredacted Claude transcripts. We aim to acknowledge a report within three business days
and provide an initial assessment within seven; complex or upstream issues may take longer.

We will coordinate disclosure and credit with the reporter. Please allow a reasonable remediation
window before publishing details that could put users' source code or credentials at risk.

## Threat model

A consenting user asks reporch to run one model session per selected repository. Each child runs
with the user's operating-system identity and may invoke shell commands, edit files and use the
network. The primary hostile inputs are:

- instructions hidden in repository files, dependencies, issue text or web content;
- a malicious or compromised repository sending a brief to another repository;
- malformed Claude stream events or local task state;
- repository hooks and test commands executed during worktree creation or merge; and
- mistakes or unexpected behavior from a powerful model operating unattended.

The likely impact includes source or secret disclosure, file modification outside the intended
worktree, arbitrary command execution as the user, unexpected network access and model spend.
reporch does not defend against an attacker who already controls the user's account or machine.

## What the guardrails do

- Every session starts in a dedicated Git worktree. The task mailbox and task-local temporary
  directory are the only additional paths reporch intentionally gives it.
- Claude settings remove direct cross-session tools, block push and pull-request creation, deny
  direct writes to known sensitive and system-temporary paths, and disable inherited MCP servers
  and slash commands.
- Brief bodies are delimited and labelled untrusted. The system prompt says a peer request cannot
  expand permissions, alter settings or authorize work outside that session's repository.
- The supervisor owns child processes, records mailbox traffic and session streams, rate-limits
  attention, applies per-session and task spend ceilings, and tears children down on completion or
  cancellation.
- No repository is merged without a literal `y` from the human merge screen. The user sees the
  actual diff first; reporch has no `--yes` path.

These controls reduce accidents and make behavior reviewable. They are not a security sandbox.

## What the guardrails do not do

`--add-dir` is additive: it makes the task mailbox available but does not revoke access elsewhere.
`bypassPermissions` still gives the session everything its Unix user can reach. A sufficiently
determined `Bash` command can evade path-oriented deny rules, use the network, read another local
repository or inspect user-owned files unless the operating system separately prevents it.

Likewise, a spend ceiling is checked between model iterations and can overshoot by one iteration.
A Git hook or configured test command is executable code from the repository. Worktree separation
does not make either of those safe.

The brief protocol limits lateral prompt injection; it cannot prove that model-generated content
is benign. One session may copy hostile text into a brief and amplify it to its peers. Mailbox logs
exist so the operator can audit that flow, not because reporch can perfectly classify it.

## What consent means

When a user accepts the project consent screen, they are trusting:

- the selected repositories, their hooks and their configured test commands;
- Claude Code and the selected models to follow reporch's system instructions and deny rules;
- their own OS account not to expose secrets that an unattended process must never see; and
- themselves to review each diff before authorizing a merge.

Use a low budget on the first run. Do not select untrusted repositories, carry secret files unless
the task requires them, or run reporch from a privileged account. For stronger isolation, run the
entire application inside a separately configured VM or OS sandbox with only the intended
repositories and credentials mounted; reporch does not create that boundary for you.

The implementation details behind these claims are specified in
[`enhanced_brief.md` §12](enhanced_brief.md#12-security-model).
