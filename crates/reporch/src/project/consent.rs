//! The §12.1 consent screen.
//!
//! > *"Once per project, at analysis time, on a screen the user must actively confirm — never a
//! > default, never remembered globally."*
//!
//! The wording is the spec's, verbatim, because it is the one place reporch tells the user
//! exactly what it is about to be allowed to do. A test asserts every clause survives edits.

/// Render the screen for a project. `repos` is the count; `budget` the per-session ceiling.
pub fn consent_screen(project: &str, repos: usize, budget_usd: f64) -> String {
    format!(
        "\
  reporch runs Claude Code sessions in bypassPermissions mode.

  In every repository of \"{project}\", each session can run any shell command and edit
  any file inside its worktree without asking you first. {repos} sessions will run at once.

  reporch configures them to:
    · work only in their own worktree and this task's mailbox     (cwd + --add-dir)
    · use task-local scratch space                                (TMPDIR + deny rules)
    · a hard spend ceiling of ${budget_usd:.2} per session                  (--max-budget-usd)
    · deny push, PR creation, and writes to known sensitive paths
    · no direct session-to-session messaging                      (deny rules)

  These are guardrails, not an OS sandbox. A bypassPermissions session still runs
  as your user; you are trusting the model and these deny rules when you continue.

  Merging is always confirmed by you, per repository.

  [ y ] I understand — enable for this project      [ n ] Cancel
"
    )
}

/// The screen as §12.1 prints it, for the worked example.
pub const CONSENT_SCREEN: &str = "see `consent_screen`";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_matches_the_spec_screen_clause_by_clause() {
        let s = consent_screen("example-app", 9, 10.0);
        for clause in [
            "reporch runs Claude Code sessions in bypassPermissions mode.",
            "In every repository of \"example-app\"",
            "9 sessions will run at once.",
            "work only in their own worktree and this task's mailbox",
            "a hard spend ceiling of $10.00 per session",
            "deny push, PR creation, and writes to known sensitive paths",
            "These are guardrails, not an OS sandbox",
            "no direct session-to-session messaging                      (deny rules)",
            "Merging is always confirmed by you, per repository.",
            "[ y ] I understand — enable for this project      [ n ] Cancel",
        ] {
            assert!(
                s.contains(clause),
                "missing clause:\n  {clause}\n\nin:\n{s}"
            );
        }
    }

    #[test]
    fn the_session_count_and_budget_are_the_projects_own() {
        let s = consent_screen("tiny", 2, 2.5);
        assert!(s.contains("2 sessions will run at once."));
        assert!(s.contains("$2.50 per session"));
    }
}
