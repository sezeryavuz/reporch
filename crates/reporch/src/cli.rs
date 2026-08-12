//! The command-line surface.
//!
//! Two audiences share one binary (§7):
//!
//! - **A person**, who runs `reporch` with no arguments and gets the TUI.
//! - **A language model with no memory of the documentation**, running `reporch brief send` from
//!   inside a session's `Bash` tool.
//!
//! The second audience is why `--json` is on everything readable, why exit codes are meaningful,
//! and why every refusal prints the command that *would* have worked.

use clap::{Parser, Subcommand};

/// Exit codes (§7.2). These are read by a model, so they must be stable and meaningful.
///
/// The whole scheme is defined here even though the subcommands that return the protocol codes
/// land in M4 — the numbers are part of the published contract in §7.2 and are asserted by the
/// tests below, so they are defined once rather than invented later.
#[allow(
    dead_code,
    reason = "the §7.2 exit-code scheme is a published contract; M4 wires the rest"
)]
pub mod exit {
    /// Success.
    pub const OK: i32 = 0;
    /// A general failure.
    pub const ERROR: i32 = 1;
    /// **A protocol violation** — e.g. a worker calling `reporch ping` (§6.2).
    pub const PROTOCOL: i32 = 2;
    /// The thing asked for does not exist.
    pub const NOT_FOUND: i32 = 3;
    /// The thing exists but is in the wrong state for this operation.
    pub const STATE_CONFLICT: i32 = 4;
}

#[derive(Debug, Parser)]
#[command(
    name = "reporch",
    version,
    about = "One task, every repo, one console.",
    long_about = "reporch runs a single development task across every repository of a multi-repo \
                  project at once, by orchestrating a Claude Code session inside each one.\n\n\
                  Run `reporch` with no arguments for the interactive console. The subcommands \
                  below are the agent surface: they are invoked by the sessions reporch spawns, \
                  and they resolve their context from the REPORCH_* environment.",
    disable_help_subcommand = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Override where reporch keeps its data. Equivalent to REPORCH_HOME.
    #[arg(long, global = true, value_name = "DIR")]
    pub home: Option<std::path::PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Check that this machine can run reporch, and say exactly what to fix if it cannot.
    Doctor {
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },

    /// Print the orchestration protocol. The escape hatch when a session is unsure of the rules.
    Protocol {
        #[arg(long)]
        json: bool,
    },

    /// Who am I: role, repo, task, master, worktree, mailbox path (§7.2).
    Whoami {
        #[arg(long)]
        json: bool,
    },

    /// The project map: every repo, its role, its worktree, its current state.
    Repos {
        #[arg(long)]
        json: bool,
    },

    /// Write and read briefs — the only channel repos use to talk to each other (§6).
    #[command(subcommand)]
    Brief(BriefCommand),

    /// Deliver attention to a repo. **Master only** (§6.2).
    Ping {
        /// The repo to wake.
        repo: String,
        /// Why, for the audit log.
        #[arg(long)]
        reason: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// Report this repo's state to the run screen.
    Status {
        #[arg(long, value_name = "STATE")]
        set: Option<String>,
        #[arg(long)]
        note: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// Task-level operations.
    #[command(subcommand)]
    Task(TaskCommand),

    /// Deliberately panic mid-render, to prove the terminal is restored (M1 acceptance).
    #[command(hide = true)]
    PanicTest,
}

#[derive(Debug, Subcommand)]
pub enum BriefCommand {
    /// File a brief for another repo. Anyone may do this — a brief is content, not attention.
    Send {
        /// The recipient repo.
        #[arg(long)]
        to: String,
        /// request | reply | fyi | blocker | question
        #[arg(long)]
        kind: String,
        #[arg(long)]
        subject: String,
        /// Read the body from a file.
        #[arg(long, value_name = "FILE", conflicts_with = "body")]
        body_file: Option<std::path::PathBuf>,
        /// The body, or `-` to read stdin.
        #[arg(long)]
        body: Option<String>,
        /// The brief this replies to.
        #[arg(long, value_name = "ID")]
        in_reply_to: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// Briefs addressed to this repo.
    Inbox {
        /// Only briefs not yet read.
        #[arg(long)]
        unread: bool,
        #[arg(long)]
        json: bool,
    },

    /// Briefs this repo has sent.
    Outbox {
        #[arg(long)]
        json: bool,
    },

    /// Read one brief in full. The body is wrapped as untrusted input (§12.2).
    Read {
        /// The brief ID, e.g. 0003.
        id: String,
        #[arg(long)]
        json: bool,
    },

    /// Close out a brief addressed to this repo.
    Ack {
        id: String,
        /// done | blocked | rejected
        #[arg(long)]
        status: String,
        #[arg(long)]
        note: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum TaskCommand {
    /// Declare the task complete and write the result report. **Master only** (§7.2).
    Finish {
        #[arg(long, value_name = "FILE")]
        report_file: std::path::PathBuf,
        #[arg(long)]
        json: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn no_arguments_means_the_tui() {
        let cli = Cli::try_parse_from(["reporch"]).unwrap();
        assert!(cli.command.is_none());
    }

    #[test]
    fn doctor_takes_json() {
        let cli = Cli::try_parse_from(["reporch", "doctor", "--json"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Doctor { json: true })));
    }

    #[test]
    fn brief_send_matches_the_spec_signature() {
        // §6.2's refusal message tells a worker to run exactly this. It must parse.
        let cli = Cli::try_parse_from([
            "reporch",
            "brief",
            "send",
            "--to",
            "modulex-js",
            "--kind",
            "request",
            "--subject",
            "Please ping modulex-js for brief 0007",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Brief(BriefCommand::Send {
                to, kind, subject, ..
            })) => {
                assert_eq!(to, "modulex-js");
                assert_eq!(kind, "request");
                assert!(subject.starts_with("Please ping"));
            }
            other => panic!("wrong parse: {other:?}"),
        }
    }

    #[test]
    fn brief_body_and_body_file_are_mutually_exclusive() {
        assert!(Cli::try_parse_from([
            "reporch",
            "brief",
            "send",
            "--to",
            "a",
            "--kind",
            "fyi",
            "--subject",
            "s",
            "--body",
            "x",
            "--body-file",
            "f",
        ])
        .is_err());
    }

    #[test]
    fn ping_takes_a_repo_and_an_optional_reason() {
        let cli = Cli::try_parse_from(["reporch", "ping", "modulex-js", "--reason", "brief 0007"])
            .unwrap();
        match cli.command {
            Some(Command::Ping { repo, reason, .. }) => {
                assert_eq!(repo, "modulex-js");
                assert_eq!(reason.as_deref(), Some("brief 0007"));
            }
            other => panic!("wrong parse: {other:?}"),
        }
    }

    /// §11.2: *"Every merge step is human-confirmed. No exceptions, no `--yes` flag."* This test
    /// exists so that adding one to make something pass fails here first.
    #[test]
    fn there_is_no_yes_flag_anywhere() {
        fn walk(cmd: &clap::Command, path: &str) {
            for a in cmd.get_arguments() {
                let id = a.get_id().as_str();
                assert!(
                    id != "yes" && id != "force" && id != "no_confirm",
                    "`{path}` has a `--{id}` flag. Merging is human-confirmed by design (§11.2); \
                     if a test needs this flag, the test is wrong."
                );
            }
            for sub in cmd.get_subcommands() {
                walk(sub, &format!("{path} {}", sub.get_name()));
            }
        }
        walk(&Cli::command(), "reporch");
    }

    /// Every subcommand a session might parse must offer `--json` (§7.2).
    #[test]
    fn every_readable_subcommand_offers_json() {
        let root = Cli::command();
        let mut missing = Vec::new();
        fn walk(cmd: &clap::Command, path: String, missing: &mut Vec<String>) {
            let subs: Vec<_> = cmd.get_subcommands().collect();
            if subs.is_empty() {
                if cmd.get_name() == "panic-test" {
                    return;
                }
                let has_json = cmd.get_arguments().any(|a| a.get_id() == "json");
                if !has_json {
                    missing.push(path);
                }
            } else {
                for s in subs {
                    walk(s, format!("{path} {}", s.get_name()), missing);
                }
            }
        }
        for s in root.get_subcommands() {
            walk(s, s.get_name().to_string(), &mut missing);
        }
        assert!(missing.is_empty(), "these lack --json: {missing:?}");
    }

    #[test]
    fn exit_codes_match_the_spec() {
        assert_eq!(exit::OK, 0);
        assert_eq!(exit::PROTOCOL, 2);
        assert_eq!(exit::NOT_FOUND, 3);
        assert_eq!(exit::STATE_CONFLICT, 4);
    }
}
