//! `reporch` — one task, every repo, one console.
//!
//! One binary, two audiences (§7): a person running `reporch` with no arguments and getting the
//! console, and a language model running `reporch brief send` from inside a session it does not
//! remember being told about.
//!
//! The crate is a library with two thin binaries — `reporch` and its short alias `rp` (§14) —
//! so both entry points share one implementation rather than compiling the same source twice.

#![forbid(unsafe_code)]

pub mod cli;
pub mod commands;
pub mod logging;
pub mod open;
pub mod project;
pub mod task;
pub mod tui;
pub mod update;

use std::io::Write;
use std::process::ExitCode;

use clap::Parser;

use cli::{BriefCommand, Cli, Command, TaskCommand};
use reporch_store::{Config, Paths};

/// Where `★ star reporch on github` points.
pub const REPO_URL: &str = "https://github.com/sezeryavuz/reporch";

/// The real entry point, shared by both binaries.
pub fn run() -> ExitCode {
    let cli = Cli::parse();

    let paths = match resolve_paths(cli.home.clone()) {
        Ok(p) => p,
        Err(e) => return fatal(&e.to_string()),
    };

    // Logging goes to a file and nowhere else. stdout is either the alternate screen or JSON a
    // model is parsing; a stray line breaks one or the other, silently.
    logging::init(&paths.log_file());

    let config = match Config::init_home(&paths) {
        Ok(c) => c,
        Err(e) => {
            return fatal(&format!(
                "could not prepare {}: {e}",
                paths.root().display()
            ))
        }
    };

    let code = match cli.command {
        Some(Command::Doctor { json }) => commands::doctor::run(&paths, json),
        Some(Command::Protocol { json }) => commands::protocol::run(json),
        Some(Command::PanicTest) => {
            // Not reached: the panic fires inside the draw call.
            let _ = tui::panic_test();
            cli::exit::OK
        }
        Some(other) => run_agent(other),
        None => return run_console(paths, config),
    };
    ExitCode::from(code as u8)
}

fn resolve_paths(explicit: Option<std::path::PathBuf>) -> Result<Paths, reporch_store::StoreError> {
    match explicit {
        Some(p) => Ok(Paths::at(p)),
        None => Paths::resolve(),
    }
}

fn run_console(paths: Paths, config: Config) -> ExitCode {
    // §5.1: doctor runs implicitly before anything else and blocks on a hard failure. A stack
    // trace three screens into the TUI is not an acceptable way to learn Claude Code is missing.
    let d = commands::doctor::check(&paths);
    if d.is_fatal() {
        let mut err = std::io::stderr().lock();
        let _ = writeln!(err, "\n  reporch cannot run on this machine yet.");
        let _ = write!(err, "{}", commands::doctor::to_text(&d));
        let _ = writeln!(err, "  Run `reporch doctor` at any time to re-check.\n");
        return ExitCode::from(cli::exit::ERROR as u8);
    }

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("reporch-runtime")
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => return fatal(&format!("could not start the async runtime: {error}")),
    };

    match runtime.block_on(tui::run(paths, config)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fatal(&format!("terminal error: {e}")),
    }
}

/// Dispatch the agent surface (§7). Every one of these resolves its context from the `REPORCH_*`
/// environment, so a session never passes a task ID or a path.
fn run_agent(cmd: Command) -> i32 {
    let ctx = match commands::agent::Ctx::from_env() {
        Ok(c) => c,
        Err(e) => {
            let mut err = std::io::stderr().lock();
            let _ = writeln!(err, "error: {e}");
            return cli::exit::ERROR;
        }
    };
    use commands::agent as a;
    match cmd {
        Command::Whoami { json } => a::whoami(&ctx, json),
        Command::Repos { json } => a::repos(&ctx, json),
        Command::Ping { repo, reason, json } => a::ping(&ctx, &repo, reason.as_deref(), json),
        Command::Status { set, note, json } => {
            a::status(&ctx, set.as_deref(), note.as_deref(), json)
        }
        Command::Brief(b) => match b {
            BriefCommand::Send {
                to,
                kind,
                subject,
                body_file,
                body,
                in_reply_to,
                json,
            } => a::brief_send(
                &ctx,
                &to,
                &kind,
                &subject,
                body_file.as_ref(),
                body.as_deref(),
                in_reply_to.as_deref(),
                json,
            ),
            BriefCommand::Inbox { unread, json } => a::brief_inbox(&ctx, unread, json),
            BriefCommand::Outbox { json } => a::brief_outbox(&ctx, json),
            BriefCommand::Read { id, json } => a::brief_read(&ctx, &id, json),
            BriefCommand::Ack {
                id,
                status,
                note,
                json,
            } => a::brief_ack(&ctx, &id, &status, note.as_deref(), json),
        },
        Command::Task(TaskCommand::Finish { report_file, json }) => {
            a::task_finish(&ctx, &report_file, json)
        }
        // Handled before this function.
        Command::Doctor { .. } | Command::Protocol { .. } | Command::PanicTest => cli::exit::OK,
    }
}

fn fatal(msg: &str) -> ExitCode {
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "error: {msg}");
    ExitCode::from(cli::exit::ERROR as u8)
}
