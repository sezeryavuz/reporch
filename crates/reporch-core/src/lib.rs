//! # reporch-core
//!
//! The domain and the protocol state machine. **This crate performs no I/O.**
//!
//! That constraint is the point of the crate, not an aesthetic preference: the protocol
//! transition is `(TaskState, Event) -> (TaskState, Vec<Action>)`, and it has to be exercisable in
//! milliseconds against a recorded event log, with no Claude process and no filesystem. If a
//! protocol rule is hard to test here, it is in the wrong crate (CLAUDE.md).
//!
//! Enforced mechanically by `tests/no_io.rs`, which fails the build if a dependency that can
//! touch the filesystem, the network, the clock or a process ever appears in this crate's tree.
//!
//! ## What lives here
//!
//! | Module | Spec |
//! | :--- | :--- |
//! | [`effort`] | the master → worker effort ladder (§5.6) |
//! | [`ids`] | task IDs, slugs, branch and directory names (§8) |
//! | [`model`] | the persisted shapes: project, task, repo (§8.1, §8.2) |
//! | [`brief`] | brief format, statuses, the index (§6.5, §8.3) |
//! | [`graph`] | the producer/consumer dependency graph (§5.4, §5.7, §11.1) |
//! | [`protocol`] | who may ping whom, and the control-plane nudge (§6) |

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod brief;
pub mod effort;
pub mod graph;
pub mod ids;
pub mod model;
pub mod protocol;

pub use brief::{
    AckStatus, Brief, BriefEntry, BriefHeader, BriefId, BriefIndex, BriefKind, BriefStatus,
    IndexEntry, PingBy, PingEntry,
};
pub use effort::{Effort, EffortPlan};
pub use graph::{DepGraph, MergeOrder, OrderViolation};
pub use ids::{branch_name, slugify, task_dir_name, TaskId};
pub use model::{
    CarryMode, CarryOver, GraphNode, ProjectBase, ProjectRepo, ProjectSettings, RepoProfile,
    RepoState, Role, Task, TaskRepo, TaskStatus, TokenUsage, PROJECT_SCHEMA_VERSION,
    TASK_SCHEMA_VERSION,
};
pub use protocol::{Action, Event, PingDecision, PingLimits, ProtocolError, TaskState, Violation};
