//! # reporch-claude
//!
//! Everything that touches Claude Code: spawning sessions, the `stream-json` codec, the session
//! registry, and `reporch doctor`.
//!
//! ## Boundary
//!
//! This crate knows how to construct and own one Claude process; it does not decide which
//! repository is master, who may ping, or when a task is complete. Those are protocol decisions
//! in `reporch-core`, interpreted by the binary's supervisor.
//!
//! [`spawn`] builds the complete, reviewable command and environment. [`session`] owns stdin and
//! stdout, appends the raw stream to a task-local JSONL file, and emits typed [`SessionEvent`]s.
//! [`events`] is deliberately tolerant of unknown fields and event kinds because Claude Code's
//! stream is an external versioned interface. The first `system/init` is still checked strictly
//! for the two tools reporch must have removed.
//!
//! Process ownership is structural: transports use kill-on-drop, and callers keep session pumps
//! in an owned Tokio `JoinSet`. Registry reads are for orphan diagnostics only; liveness comes
//! from the stream that reporch already owns.

#![forbid(unsafe_code)]

pub mod doctor;
pub mod events;
pub mod registry;
pub mod session;
pub mod spawn;
pub mod version;

pub use doctor::{diagnose, Check, Diagnosis, Env, Severity};
pub use events::{parse_line, Init, StreamEvent, TurnResult};
pub use registry::{live_sessions, LiveSession};
pub use session::{launch, run_session, ChildTransport, ReplayTransport, SessionEvent, Transport};
pub use spawn::{reporch_bin_path, SessionSpec, SpawnCommand};
pub use version::Version;

/// The tools every spawned session has removed (§6.2).
///
/// Denying by **bare tool name** removes the tool from the model's context entirely — it never
/// sees it. A scoped rule like `Bash(rm *)` would only block the call. Spike #8 confirmed the
/// removal empirically: 60 tools without these rules, 58 with.
pub const DENIED_TOOLS: [&str; 2] = ["SendMessage", "ListAgents"];

#[derive(Debug, thiserror::Error)]
pub enum ClaudeError {
    #[error("could not run `claude`: {0}")]
    Spawn(#[source] std::io::Error),

    #[error("session for `{repo}` exited before it initialised")]
    DiedBeforeInit { repo: String },

    /// The §4.4 spawn assertion failed. Not recoverable: the protocol cannot be enforced.
    #[error(
        "session for `{repo}` still has {} available. The --settings deny rules did not take \
         effect — note that `-p` silently ignores a settings payload that fails validation. \
         The protocol cannot be enforced, so the task will not start.",
        tools.join(" and ")
    )]
    DenyRulesNotApplied { repo: String, tools: Vec<String> },

    #[error("{0}")]
    Other(String),
}
