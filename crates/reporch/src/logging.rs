//! Logging setup.
//!
//! **To a file only.** Never stdout, never stderr (CLAUDE.md, §13.2):
//!
//! - stdout is the alternate screen while the TUI is up; a log line drawn into it corrupts the
//!   frame.
//! - stdout is also the `--json` output that a *language model* parses when it runs
//!   `reporch brief inbox --json`. A stray log line there is a parse failure inside an agent.
//!
//! Both failure modes are silent and confusing, which is why this is a hard rule rather than a
//! preference.

use std::fs::OpenOptions;
use std::path::Path;

use tracing_subscriber::EnvFilter;

/// Initialise tracing to `~/.reporch/logs/reporch.log`.
///
/// Failure to open the log file is deliberately **not** fatal: not being able to write a log is
/// no reason to refuse to run, and the alternative — falling back to stderr — is exactly what
/// this module exists to prevent.
pub fn init(log_file: &Path) {
    if let Some(dir) = log_file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Ok(file) = OpenOptions::new().create(true).append(true).open(log_file) else {
        return;
    };
    let filter = EnvFilter::try_from_env("REPORCH_LOG")
        .unwrap_or_else(|_| EnvFilter::new("reporch=info,reporch_core=info,reporch_claude=info,reporch_store=info,reporch_git=info,reporch_scan=info"));

    // `with_ansi(false)`: the file is read with `tail`, `less` and an editor, none of which want
    // escape codes.
    let _ = tracing_subscriber::fmt()
        .with_writer(file)
        .with_ansi(false)
        .with_target(true)
        .with_env_filter(filter)
        .try_init();
}
