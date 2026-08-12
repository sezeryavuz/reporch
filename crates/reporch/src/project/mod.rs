//! Creating and reading a project (§5.2–§5.5, §8.1).

pub mod analyse;
pub mod catalog;
pub mod consent;

pub use analyse::synthesis_prompt;
pub use analyse::{analyse_repo, assemble, render_markdown, AnalysisOutcome};
pub use consent::CONSENT_SCREEN;
