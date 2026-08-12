<div align="center">
<pre>
 ██████╗ ███████╗██████╗  ██████╗ ██████╗  ██████╗██╗  ██╗
 ██╔══██╗██╔════╝██╔══██╗██╔═══██╗██╔══██╗██╔════╝██║  ██║
 ██████╔╝█████╗  ██████╔╝██║   ██║██████╔╝██║     ███████║
 ██╔══██╗██╔══╝  ██╔═══╝ ██║   ██║██╔══██╗██║     ██╔══██║
 ██║  ██║███████╗██║     ╚██████╔╝██║  ██║╚██████╗██║  ██║
 ╚═╝  ╚═╝╚══════╝╚═╝      ╚═════╝ ╚═╝  ╚═╝ ╚═════╝╚═╝  ╚═╝
</pre>

**One task. Every repo. One console.**

Run one development task across every repository in a multi-repo project, with a governed Claude
Code session working inside each one.

[![CI](https://github.com/sezeryavuz/reporch/actions/workflows/ci.yml/badge.svg)](https://github.com/sezeryavuz/reporch/actions/workflows/ci.yml)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![crates.io: unpublished](https://img.shields.io/badge/crates.io-unpublished-lightgrey.svg)](#install)
[![MSRV: Rust 1.85](https://img.shields.io/badge/MSRV-Rust%201.85-dea584.svg)](rust-toolchain.toml)
[![platforms: macOS and Linux](https://img.shields.io/badge/platforms-macOS%20%7C%20Linux-6f42c1.svg)](#requirements)

</div>

> [!IMPORTANT]
> reporch is a working, source-buildable alpha. It has completed and merged a real task across
> three repositories, and 455 local tests cover its protocol, console, processes, persistence and
> real Git behavior. It has not been published to crates.io, npm or Homebrew, and the release
> workflow has never been executed.

## See the whole project move

```text
  TASK-0001  widget-status                   running · 6m12s · $1.84 / $10.00

  REPO                ROLE     STATE      LAST                         BRIEFS       COST
› example-app         master   working    editing app/widgets.py       ─            $0.90
  example-app-ui      worker   working    reading brief 0003            1 in         $0.50
  example-app-python  worker   done       replied to brief 0002         1 in 1 out   $0.44
  example-app-js      worker   waiting    idle 2m                        1 in         $0.00
  example-app-docs    worker   idle       ─                              ─            $0.00

  `example-app-ui` pinged by reporch — brief 0003 filed

  enter attach · b briefs · l logs · p pause attention · x abort
```

This is a static render of the tested run-screen layout. A terminal recording belongs here after
the first published build can be installed from a clean machine.

## Your project is not one repository

A backend change becomes a web change, two SDK changes and a docs change. Today that means
repeating the task in separate terminals, carrying decisions between contexts, and discovering
drift at merge time. reporch gives the project one brief, lets each repository work in its own Git
worktree, and keeps delegation, spend, progress and merge decisions visible in one terminal.

## How it works

Content and attention travel on separate paths. That distinction is the control system, not a
prompt convention:

```text
                         content: durable, inspectable files

  ┌────────┐    brief     ┌────────────────┐     brief     ┌──────────┐
  │ master │─────────────►│  task mailbox  │◄──────────────│ worker A │
  └───┬────┘              └───────┬────────┘               └────┬─────┘
      │                            │ recipient reads             │
      │ ping request               ▼                             │ needs attention
      │                    ┌──────────────┐                       │ brief to master
      └───────────────────►│   reporch    │◄──────────────────────┘
                           │ control plane│
                           └──────┬───────┘
                                  │ rate-limited stdin wake-up
                                  ▼
                             ┌──────────┐
                             │ worker B │
                             └──────────┘
```

- Anyone may write a brief to anyone.
- Only the master may request a ping. Workers never ping a peer.
- reporch delivers approved attention over the session's stdin, rate-limited and logged.
- Claude's direct `SendMessage` and `ListAgents` tools are removed; the role-aware `reporch` CLI
  refuses protocol violations in code and prints the safe alternative.

The result is collaboration you can reconstruct: every brief is readable, every ping has a reason,
and every raw session stream is retained until a successful merge compacts the task.

## The workflow

1. **Create a project.** reporch scans for Git repositories, ranks related checkouts together, and
   asks you to select the set. After explicit permission consent, Claude analyzes each repository
   and builds the producer/consumer graph used for leadership and merge order.
2. **Describe one task.** Choose the model, effort and master repository. reporch creates a sibling
   worktree for every repository and starts one budget-bounded session in each.
3. **Watch the run.** Open a selected session's stream, the chronological mailbox or all logs.
   Pause attention delivery without hiding spend or terminal events; abort with confirmation while
   preserving worktrees.
4. **Review and merge.** The master submits a structured result. reporch shows each real diff and
   test command in dependency order. A literal `y` authorizes one repository—never the whole task.

## Quick start from source

Nothing needs to be published for this path to work:

```bash
git clone https://github.com/sezeryavuz/reporch.git
cd reporch
cargo build --workspace --release --locked

./target/release/reporch doctor
./target/release/reporch
```

In the console:

1. Choose `new project`, name it, select repositories with `space`, and read the permission screen
   before pressing `y`.
2. Open the project and choose `new task`.
3. Enter the task brief, select model and effort, review the elected master, then start the run.

Project analysis and task runs invoke real Claude Code sessions and spend against your account.
Start with a small per-session budget and repositories you trust.

The build also produces `./target/release/rp`, a full second binary with the same behavior as
`reporch`.

## Install

**No install channel is live yet.** The cargo-dist workflow is committed for review but has never
been run. After the first human-cut release, the intended channels are:

```bash
brew install sezeryavuz/tap/reporch
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/sezeryavuz/reporch/releases/latest/download/reporch-installer.sh | sh
cargo install reporch --locked
npx reporch
```

Until release artifacts exist, use the source quick start above. No command in this section is
claimed to work today.

## Safety is part of the product

reporch uses Claude Code's `bypassPermissions` mode. That is what lets unattended sessions edit,
test and coordinate, and it means the sessions run with your operating-system identity.

The application adds several enforceable and inspectable boundaries:

- Project consent is explicit, stored and repeated when the repository set grows.
- Sessions work from dedicated Git worktrees, use task-local temporary storage, and receive only
  the task mailbox as an additional intended directory.
- Direct messaging tools, inherited MCP servers, push, PR creation and direct writes to known
  sensitive paths are denied.
- Briefs are delimited as untrusted peer input; they cannot grant permission or expand scope.
- Per-session and task ceilings bound model spend, and the live screen shows cumulative cost.
- The supervisor owns every child process and tears it down on completion, cancellation or error.
- Merging is repository-by-repository and requires a literal human `y`. There is no `--yes` flag.

These are guardrails, not an OS sandbox. `--add-dir` adds access; it does not remove access the
user already has. A `Bash` command can reach paths or the network unless the OS or a specific deny
rule stops it, and model budget checks can overshoot by one iteration. Read the full
[security policy](SECURITY.md) before consenting.

## Plain files, not a black box

`REPORCH_HOME` defaults to `~/.reporch`. State is JSON and Markdown; session streams are JSONL.

```text
~/.reporch/
├── config.toml
├── logs/reporch.log
├── worktrees/example-app/TASK-0001/
│   ├── example-app/
│   └── example-app-ui/
└── projects/example-app/
    ├── project_base.json         repositories, settings, dependency graph
    ├── project_base.md           readable project overview
    ├── repos/
    │   ├── example-app.json
    │   └── example-app.md
    └── tasks/widget-status-TASK-0001/
        ├── task.json             lifecycle, roles, worktrees, sessions, spend
        ├── base.md               the operator's brief, verbatim
        ├── protocol.md           exact rules injected into every session
        ├── briefs/               mailbox files plus chronological index
        ├── logs/                 raw stream-json, one file per repository
        ├── result.{json,md}      structured completion plus readable report
        └── merge.json            checkpointed human decisions
```

Set `REPORCH_HOME` or pass `--home` to isolate a development or diagnostic run.

## Requirements

- macOS or Linux on x86_64 or arm64
- Git
- Claude Code `>= 2.1.224`
- Rust 1.85 or newer when building from source; `rust-toolchain.toml` pins contributor checks to
  Rust 1.95.0

Native Windows is unsupported because Claude Code cross-session messaging is unavailable there;
WSL 2 is the Windows route. Amazon Bedrock, Claude Platform on AWS, Google Cloud Agent Platform
and Microsoft Foundry are not supported by the underlying feature. `reporch doctor` checks the
machine and gives a specific remedy for every blocking condition it can detect.

## Non-goals for v1

- Not a monorepo task runner. reporch coordinates separate repositories and their worktrees.
- Not a cloud service, web UI or MCP server. It is a local terminal application.
- Not a general provider abstraction. v1 is built around Claude Code's process and stream
  behavior.
- Not native Windows support.
- Not automatic merging. Human confirmation is the design.

## Read the engineering record

- [Architecture](docs/architecture.md) — crates, actor loop, pure state machine and persistence.
- [Protocol](docs/protocol.md) — the exact text sessions receive and the CLI prints.
- [Troubleshooting](docs/troubleshooting.md) — known failure modes and safe recovery.
- [Live analysis proof](docs/analysis-verification.md) — generated profiles, graph, and limitations.
- [Live console-task proof](docs/task-verification.md) — paid completion, controls, accounting, and teardown.
- [Product specification](enhanced_brief.md) — the numbered contract, including every Deviation.
- [Claude Code spikes](docs/spikes.md) — ten paid experiments and their committed fixtures.
- [Decisions](docs/decisions.md) — every consequential fork and why it was taken.

## Contributing

Start with [CONTRIBUTING.md](CONTRIBUTING.md). Behavior changes cite the relevant specification
section, tests use real repositories/processes or committed captures rather than mocks, and any
new contradiction with reality becomes a visible Deviation.

Please follow the [Code of Conduct](CODE_OF_CONDUCT.md). Report vulnerabilities privately as
described in [SECURITY.md](SECURITY.md).

## License

reporch is available under the [MIT License](LICENSE).
