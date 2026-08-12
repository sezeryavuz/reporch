//! `reporch doctor` — rendering (§4.5, §5.1).
//!
//! The diagnosis itself lives in `reporch-claude`, where it is pure and fully unit-tested. This
//! module only decides how it looks.

use std::io::Write;

use reporch_claude::{diagnose, Diagnosis, Env, Severity};
use reporch_store::Paths;

/// Run the checks and print them. Returns the process exit code.
pub fn run(paths: &Paths, json: bool) -> i32 {
    let env = Env::detect(Some(paths.root().to_path_buf()));
    let d = diagnose(&env);
    let out = std::io::stdout();
    let mut out = out.lock();
    if json {
        let _ = writeln!(out, "{}", to_json(&d));
    } else {
        let _ = write!(out, "{}", to_text(&d));
    }
    d.exit_code()
}

/// Run the checks without printing. Used by the implicit first-run check (§5.1).
pub fn check(paths: &Paths) -> Diagnosis {
    diagnose(&Env::detect(Some(paths.root().to_path_buf())))
}

pub fn to_json(d: &Diagnosis) -> String {
    let checks: Vec<serde_json::Value> = d
        .checks
        .iter()
        .map(|c| {
            serde_json::json!({
                "id": c.id,
                "title": c.title,
                "severity": c.severity.symbol(),
                "detail": c.detail,
                "fix": c.fix,
            })
        })
        .collect();
    serde_json::json!({
        "ok": !d.is_fatal(),
        "severity": d.severity().symbol(),
        "exit_code": d.exit_code(),
        "checks": checks,
    })
    .to_string()
}

pub fn to_text(d: &Diagnosis) -> String {
    let mut s = String::new();
    s.push('\n');
    let width = d.checks.iter().map(|c| c.title.len()).max().unwrap_or(0);

    for c in &d.checks {
        let mark = match c.severity {
            Severity::Ok => "  ok  ",
            Severity::Warn => "  warn",
            Severity::Fail => "  FAIL",
        };
        s.push_str(&format!(
            "{mark}  {:width$}  {}\n",
            c.title,
            c.detail,
            width = width
        ));
    }

    // The remedies come after the summary, indented, so a passing run is four lines and a
    // failing one tells you exactly what to do.
    let needs_action: Vec<_> = d
        .checks
        .iter()
        .filter(|c| c.severity != Severity::Ok)
        .collect();
    if !needs_action.is_empty() {
        s.push('\n');
        for c in needs_action {
            if let Some(fix) = &c.fix {
                s.push_str(&format!("  {}:\n", c.title));
                for line in wrap(fix, 76) {
                    s.push_str(&format!("    {line}\n"));
                }
                s.push('\n');
            }
        }
    }

    s.push_str(match d.severity() {
        Severity::Ok => "  Everything reporch needs is here.\n\n",
        Severity::Warn => "  reporch will run, with the caveats above.\n\n",
        Severity::Fail => "  reporch cannot run until the failures above are fixed.\n\n",
    });
    s
}

/// Wrap on word boundaries. Doctor output is prose the user reads under stress.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use reporch_claude::Check;

    fn diag(checks: Vec<Check>) -> Diagnosis {
        Diagnosis { checks }
    }

    #[test]
    fn json_is_parseable_and_carries_the_exit_code() {
        let d = check_fixture();
        let v: serde_json::Value = serde_json::from_str(&to_json(&d)).unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["exit_code"], 1);
        assert_eq!(v["severity"], "FAIL");
        assert_eq!(v["checks"].as_array().unwrap().len(), 2);
        assert!(v["checks"][1]["fix"].is_string());
    }

    #[test]
    fn text_shows_every_check_and_every_remedy() {
        let t = to_text(&check_fixture());
        assert!(t.contains("git"), "{t}");
        assert!(t.contains("FAIL"), "{t}");
        assert!(t.contains("Install git"), "the remedy must appear: {t}");
        assert!(t.contains("cannot run"), "{t}");
    }

    #[test]
    fn a_passing_run_is_short_and_says_so() {
        let d = diag(vec![Check {
            id: "x",
            title: "Thing".into(),
            severity: Severity::Ok,
            detail: "fine".into(),
            fix: None,
        }]);
        let t = to_text(&d);
        assert!(t.contains("Everything reporch needs is here"));
        assert!(!t.contains("FAIL"));
        assert!(
            t.lines().filter(|l| !l.trim().is_empty()).count() <= 3,
            "{t}"
        );
    }

    #[test]
    fn warnings_do_not_claim_failure() {
        let d = diag(vec![Check {
            id: "x",
            title: "Flags".into(),
            severity: Severity::Warn,
            detail: "DO_NOT_TRACK is set".into(),
            fix: Some("Unset it for full functionality.".into()),
        }]);
        let t = to_text(&d);
        assert!(t.contains("will run"));
        assert!(!t.contains("cannot run"));
        assert_eq!(d.exit_code(), 0);
    }

    #[test]
    fn long_remedies_wrap_on_word_boundaries() {
        let long = "word ".repeat(60);
        let lines = wrap(&long, 40);
        assert!(lines.len() > 1);
        for l in &lines {
            assert!(l.chars().count() <= 40, "line too long: {l:?}");
            assert!(!l.starts_with(' ') && !l.ends_with(' '));
        }
    }

    #[test]
    fn wrap_handles_a_word_longer_than_the_width() {
        let lines = wrap("short averyveryverylongtokenwithoutspaces end", 10);
        assert!(lines.iter().any(|l| l.contains("averyvery")));
    }

    fn check_fixture() -> Diagnosis {
        diag(vec![
            Check {
                id: "platform",
                title: "Operating system".into(),
                severity: Severity::Ok,
                detail: "macos".into(),
                fix: None,
            },
            Check {
                id: "git",
                title: "git".into(),
                severity: Severity::Fail,
                detail: "the `git` binary could not be run".into(),
                fix: Some("Install git and make sure it is on your PATH.".into()),
            },
        ])
    }
}
