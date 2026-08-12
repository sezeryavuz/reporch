//! Creating and running a task (§5.6–§5.8, §9, §10).

pub mod create;
pub mod merge;
pub mod runtime;
pub mod supervisor;

pub use create::{create_task, kickoff_prompt, CreateRequest, Created};
pub use merge::{
    diff_summary, has_task_changes, load_report, parse_report, validate_report, Confirmation,
    LoadedReport, MergePlan, MergeStep, ResultReport, StepOutcome,
};
pub use supervisor::{FinishReason, Notice, RunRow, RunView, Supervisor};
