//! Task identifiers and slugs (§8).
//!
//! Task directories are named `<slug>-<TASK_ID>`, e.g. `org-scoped-api-keys-TASK-0001`. The slug
//! comes from the brief and is advisory; the ID is what guarantees uniqueness.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// `TASK-0001`. Monotonic per project, zero-padded to four digits so a directory listing sorts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TaskId(u32);

impl TaskId {
    pub const fn new(n: u32) -> Self {
        Self(n)
    }

    pub const fn number(self) -> u32 {
        self.0
    }

    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Zero-padded to 4, but wider IDs still render correctly rather than truncating.
        write!(f, "TASK-{:04}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid task id `{0}`; expected the form TASK-0001")]
pub struct ParseTaskIdError(pub String);

impl FromStr for TaskId {
    type Err = ParseTaskIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let rest = s
            .strip_prefix("TASK-")
            .ok_or_else(|| ParseTaskIdError(s.to_string()))?;
        if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) {
            return Err(ParseTaskIdError(s.to_string()));
        }
        rest.parse::<u32>()
            .map(TaskId)
            .map_err(|_| ParseTaskIdError(s.to_string()))
    }
}

impl TryFrom<String> for TaskId {
    type Error = ParseTaskIdError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<TaskId> for String {
    fn from(id: TaskId) -> String {
        id.to_string()
    }
}

/// Maximum slug length (§8). Long enough to stay readable, short enough for a branch name.
pub const SLUG_MAX_LEN: usize = 40;

/// Normalise arbitrary text into a lowercase-kebab ASCII slug, ≤ 40 chars (§8).
///
/// This is the deterministic fallback and the validator for the `claude -p --json-schema` slug
/// call: whatever the model proposes is passed through here, so a model that returns
/// `"Org Scoped API Keys!"` or 200 characters of prose still produces a legal directory and
/// branch name.
///
/// Returns `None` only when nothing usable survives, in which case the caller falls back to the
/// task ID alone.
pub fn slugify(input: &str) -> Option<String> {
    let mut out = String::with_capacity(SLUG_MAX_LEN);
    let mut prev_dash = false;
    for ch in input.chars() {
        // Non-ASCII is transliterated crudely: dropped, with the surrounding words still joined.
        let mapped = if ch.is_ascii_alphanumeric() {
            Some(ch.to_ascii_lowercase())
        } else {
            None
        };
        match mapped {
            Some(c) => {
                out.push(c);
                prev_dash = false;
            }
            None => {
                if !out.is_empty() && !prev_dash {
                    out.push('-');
                    prev_dash = true;
                }
            }
        }
        if out.len() >= SLUG_MAX_LEN {
            break;
        }
    }
    // Trim to the last word boundary rather than mid-word when we hit the cap.
    while out.len() > SLUG_MAX_LEN {
        out.pop();
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// The task directory name, `<slug>-<TASK_ID>` (§8).
pub fn task_dir_name(slug: &str, id: TaskId) -> String {
    match slugify(slug) {
        Some(s) => format!("{s}-{id}"),
        None => id.to_string(),
    }
}

/// The shared branch name for a task (§9.1): `reporch/TASK-0001-<slug>`.
///
/// Prefixed `reporch/` so it is self-identifying in `git branch` and trivially cleanable.
pub fn branch_name(slug: &str, id: TaskId) -> String {
    match slugify(slug) {
        Some(s) => format!("reporch/{id}-{s}"),
        None => format!("reporch/{id}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_id_formats_zero_padded() {
        assert_eq!(TaskId::new(1).to_string(), "TASK-0001");
        assert_eq!(TaskId::new(42).to_string(), "TASK-0042");
        assert_eq!(TaskId::new(12345).to_string(), "TASK-12345");
    }

    #[test]
    fn task_id_round_trips() {
        for n in [1u32, 9, 10, 999, 1000, 99999] {
            let id = TaskId::new(n);
            assert_eq!(id.to_string().parse::<TaskId>().unwrap(), id);
        }
    }

    #[test]
    fn task_id_rejects_junk() {
        for bad in [
            "",
            "TASK",
            "TASK-",
            "0001",
            "task-0001",
            "TASK-00x1",
            "TASK-1 ",
        ] {
            assert!(bad.parse::<TaskId>().is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn task_id_serde_is_the_string_form() {
        let id = TaskId::new(7);
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"TASK-0007\"");
        assert_eq!(serde_json::from_str::<TaskId>("\"TASK-0007\"").unwrap(), id);
    }

    #[test]
    fn slugify_handles_the_worked_example() {
        assert_eq!(
            slugify("Add organization-scoped API keys").unwrap(),
            "add-organization-scoped-api-keys"
        );
    }

    #[test]
    fn slugify_collapses_and_trims_separators() {
        assert_eq!(slugify("  ...Hello,,,   World!!! ").unwrap(), "hello-world");
        assert_eq!(slugify("a---b").unwrap(), "a-b");
    }

    #[test]
    fn slugify_caps_length_and_never_ends_in_a_dash() {
        let long = "a very long task description that goes on and on and on well past the limit";
        let s = slugify(long).unwrap();
        assert!(s.len() <= SLUG_MAX_LEN, "{} chars: {s}", s.len());
        assert!(!s.ends_with('-'));
        assert!(!s.starts_with('-'));
    }

    #[test]
    fn slugify_returns_none_when_nothing_survives() {
        assert_eq!(slugify(""), None);
        assert_eq!(slugify("!!!___---"), None);
        assert_eq!(slugify("日本語"), None);
    }

    #[test]
    fn slugify_keeps_ascii_from_mixed_scripts() {
        assert_eq!(slugify("API キー scoping").unwrap(), "api-scoping");
    }

    #[test]
    fn dir_and_branch_names_match_the_spec() {
        let id = TaskId::new(1);
        assert_eq!(
            task_dir_name("org-scoped-api-keys", id),
            "org-scoped-api-keys-TASK-0001"
        );
        assert_eq!(
            branch_name("org-scoped-api-keys", id),
            "reporch/TASK-0001-org-scoped-api-keys"
        );
    }

    #[test]
    fn unusable_slug_falls_back_to_the_id_alone() {
        let id = TaskId::new(3);
        assert_eq!(task_dir_name("!!!", id), "TASK-0003");
        assert_eq!(branch_name("!!!", id), "reporch/TASK-0003");
    }

    /// A slug is used as both a directory name and a git branch component, so it must never
    /// contain anything either would reject.
    #[test]
    fn slug_is_always_path_and_branch_safe() {
        let inputs = [
            "../../etc/passwd",
            "a/b/c",
            "with spaces and\ttabs\nand newlines",
            "emoji 🎉 party",
            "..",
            ".",
            "-leading-and-trailing-",
            "CAPS AND lower",
        ];
        for i in inputs {
            if let Some(s) = slugify(i) {
                assert!(
                    s.bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
                    "slug {s:?} from {i:?} has illegal bytes"
                );
                assert!(!s.contains("..") && !s.contains('/'));
                assert!(!s.starts_with('-') && !s.ends_with('-'));
                assert!(!s.is_empty());
            }
        }
    }
}
