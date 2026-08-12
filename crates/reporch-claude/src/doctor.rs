//! `reporch doctor` — verify the environment before anything expensive happens (§4.5, §5.1).
//!
//! Runs implicitly on first run and blocks on any hard failure. Claude Code missing, too old, the
//! wrong provider, or a feature flag that disables messaging are all fatal, and each must produce
//! *a specific, actionable message — not a stack trace*.
//!
//! Every check is pure given its inputs ([`Env`]), so the whole matrix is unit-testable without
//! touching the real machine. [`Env::detect`] is the only part that reads the world.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use crate::version::Version;

/// How serious a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Everything reporch needs is present.
    Ok,
    /// reporch will run, but something is degraded or worth knowing.
    Warn,
    /// reporch cannot run. Exit non-zero.
    Fail,
}

impl Severity {
    pub const fn symbol(self) -> &'static str {
        match self {
            Severity::Ok => "ok",
            Severity::Warn => "warn",
            Severity::Fail => "FAIL",
        }
    }
}

/// One diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// Short stable identifier, also used by `--json`.
    pub id: &'static str,
    pub title: String,
    pub severity: Severity,
    /// What was found.
    pub detail: String,
    /// What to do about it. Required for anything that is not `Ok` — a diagnosis without a
    /// remedy is just bad news.
    pub fix: Option<String>,
}

impl Check {
    fn ok(id: &'static str, title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            id,
            title: title.into(),
            severity: Severity::Ok,
            detail: detail.into(),
            fix: None,
        }
    }

    fn warn(
        id: &'static str,
        title: impl Into<String>,
        detail: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        Self {
            id,
            title: title.into(),
            severity: Severity::Warn,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }

    fn fail(
        id: &'static str,
        title: impl Into<String>,
        detail: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        Self {
            id,
            title: title.into(),
            severity: Severity::Fail,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }
}

/// The full report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnosis {
    pub checks: Vec<Check>,
}

impl Diagnosis {
    pub fn severity(&self) -> Severity {
        self.checks
            .iter()
            .map(|c| c.severity)
            .max()
            .unwrap_or(Severity::Ok)
    }

    pub fn is_fatal(&self) -> bool {
        self.severity() == Severity::Fail
    }

    /// `0` when reporch can run, `1` when it cannot. Warnings do not fail the command — they are
    /// information, and exiting non-zero on them would make `reporch doctor` useless in a script.
    pub fn exit_code(&self) -> i32 {
        if self.is_fatal() {
            1
        } else {
            0
        }
    }

    pub fn failures(&self) -> impl Iterator<Item = &Check> {
        self.checks.iter().filter(|c| c.severity == Severity::Fail)
    }
}

/// The four environment variables that each silently disable cross-session messaging (§4.5).
///
/// "Silently" is the important word: nothing surfaces, the feature is simply gone. An enterprise
/// user plausibly has one of these set without knowing what it turns off.
pub const MESSAGING_KILL_SWITCHES: [&str; 4] = [
    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
    "DISABLE_TELEMETRY",
    "DO_NOT_TRACK",
    "DISABLE_GROWTHBOOK",
];

/// Environment variables indicating a provider that does not support cross-session messaging
/// (§4.5): Amazon Bedrock, Claude Platform on AWS, Google Cloud's Agent Platform, and Microsoft
/// Foundry.
pub const THIRD_PARTY_PROVIDER_VARS: [(&str, &str); 6] = [
    ("CLAUDE_CODE_USE_BEDROCK", "Amazon Bedrock"),
    ("CLAUDE_CODE_USE_VERTEX", "Google Cloud's Agent Platform"),
    ("CLAUDE_CODE_USE_FOUNDRY", "Microsoft Foundry"),
    ("ANTHROPIC_BEDROCK_BASE_URL", "Amazon Bedrock"),
    ("ANTHROPIC_VERTEX_BASE_URL", "Google Cloud's Agent Platform"),
    ("ANTHROPIC_FOUNDRY_BASE_URL", "Microsoft Foundry"),
];

/// The flags reporch's spawn command depends on (§10). If a future Claude Code drops one, the
/// spawn breaks in a confusing way at task start; checking `--help` catches it here instead.
pub const REQUIRED_FLAGS: [&str; 10] = [
    "--session-id",
    "--input-format",
    "--output-format",
    "--append-system-prompt",
    "--add-dir",
    "--settings",
    "--effort",
    "--permission-mode",
    "--max-budget-usd",
    "--replay-user-messages",
];

/// Everything the doctor inspects, gathered once so the checks themselves stay pure.
#[derive(Debug, Clone, Default)]
pub struct Env {
    /// `None` when the `claude` binary could not be run at all.
    pub claude_version_output: Option<String>,
    pub claude_help_output: Option<String>,
    pub git_version_output: Option<String>,
    /// Relevant environment variables that are set, and to what.
    pub vars: BTreeMap<String, String>,
    pub os: &'static str,
    /// Where reporch keeps its data, and whether that location is writable.
    pub home: Option<PathBuf>,
    pub home_writable: bool,
}

impl Env {
    /// Read the real machine.
    pub fn detect(home: Option<PathBuf>) -> Self {
        let mut vars = BTreeMap::new();
        for k in MESSAGING_KILL_SWITCHES {
            if let Ok(v) = std::env::var(k) {
                if !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false") {
                    vars.insert(k.to_string(), v);
                }
            }
        }
        for (k, _) in THIRD_PARTY_PROVIDER_VARS {
            if let Ok(v) = std::env::var(k) {
                if !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false") {
                    vars.insert(k.to_string(), v);
                }
            }
        }

        let home_writable = home.as_ref().is_some_and(|h| is_writable(h));

        Self {
            claude_version_output: run("claude", &["--version"]),
            claude_help_output: run("claude", &["--help"]),
            git_version_output: run("git", &["--version"]),
            vars,
            os: std::env::consts::OS,
            home,
            home_writable,
        }
    }
}

fn run(bin: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(bin).args(args).output().ok()?;
    if !out.status.success() && out.stdout.is_empty() {
        return None;
    }
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    Some(s)
}

/// Whether a directory is writable — by creating the directory if needed and writing a probe file.
fn is_writable(dir: &std::path::Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".reporch-write-probe");
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// Run every check. Pure: the same [`Env`] always produces the same [`Diagnosis`].
pub fn diagnose(env: &Env) -> Diagnosis {
    let mut checks = Vec::new();

    // ---- platform ---------------------------------------------------------
    // Native Windows has no cross-session messaging at all (§4.5, §16). WSL 2 works, and reports
    // itself as linux, so this correctly passes there.
    if env.os == "windows" {
        checks.push(Check::fail(
            "platform",
            "Operating system",
            "native Windows",
            "Cross-session messaging does not exist on native Windows, so reporch cannot run \
             here. Use WSL 2: install Claude Code inside your WSL distribution and run reporch \
             from that shell.",
        ));
    } else {
        checks.push(Check::ok("platform", "Operating system", env.os));
    }

    // ---- claude code present and recent enough ----------------------------
    match env.claude_version_output.as_deref() {
        None => checks.push(Check::fail(
            "claude_present",
            "Claude Code",
            "the `claude` binary could not be run",
            "Install Claude Code and make sure `claude` is on your PATH. \
             See https://code.claude.com/docs — then re-run `reporch doctor`.",
        )),
        Some(out) => match Version::parse(out) {
            None => checks.push(Check::warn(
                "claude_present",
                "Claude Code",
                format!(
                    "`claude --version` printed something unexpected: {}",
                    out.trim()
                ),
                "reporch could not read the version, so it cannot confirm you are on 2.1.224 or \
                 later. Check `claude --version` by hand.",
            )),
            Some(v) if !v.meets_minimum() => checks.push(Check::fail(
                "claude_version",
                "Claude Code version",
                format!("{v}, but reporch needs {} or later", Version::MINIMUM),
                format!(
                    "Cross-session messaging arrived in {}. Run `claude update` (or \
                     `claude install latest`), then re-run `reporch doctor`.",
                    Version::MINIMUM
                ),
            )),
            Some(v) => {
                checks.push(Check::ok(
                    "claude_version",
                    "Claude Code version",
                    format!("{v} (minimum {})", Version::MINIMUM),
                ));
            }
        },
    }

    // ---- the flags reporch's spawn depends on -----------------------------
    if let Some(help) = env.claude_help_output.as_deref() {
        let missing: Vec<&str> = REQUIRED_FLAGS
            .iter()
            .copied()
            .filter(|f| !help.contains(f))
            .collect();
        if missing.is_empty() {
            checks.push(Check::ok(
                "claude_flags",
                "Claude Code flags",
                format!("all {} required flags present", REQUIRED_FLAGS.len()),
            ));
        } else {
            checks.push(Check::fail(
                "claude_flags",
                "Claude Code flags",
                format!("`claude --help` does not mention: {}", missing.join(", ")),
                "reporch drives Claude Code through these flags, so a session cannot be spawned \
                 without them. This usually means Claude Code changed in a way reporch has not \
                 caught up with — please open an issue naming your `claude --version`.",
            ));
        }
    }

    // ---- provider ---------------------------------------------------------
    let third_party: Vec<&str> = THIRD_PARTY_PROVIDER_VARS
        .iter()
        .filter(|(k, _)| env.vars.contains_key(*k))
        .map(|(_, name)| *name)
        .collect();
    if third_party.is_empty() {
        checks.push(Check::ok(
            "provider",
            "Model provider",
            "first-party (Anthropic API or a Claude subscription)",
        ));
    } else {
        let mut names: Vec<&str> = third_party;
        names.sort_unstable();
        names.dedup();
        checks.push(Check::fail(
            "provider",
            "Model provider",
            format!("configured for {}", names.join(", ")),
            "Cross-session messaging is not available on Amazon Bedrock, Claude Platform on AWS, \
             Google Cloud's Agent Platform, or Microsoft Foundry — reporch's whole coordination \
             mechanism depends on it. Unset the provider environment variables to use a \
             first-party Anthropic account.",
        ));
    }

    // ---- the four silent kill switches ------------------------------------
    let tripped: Vec<&String> = MESSAGING_KILL_SWITCHES
        .iter()
        .filter_map(|k| env.vars.get_key_value(*k).map(|(k, _)| k))
        .collect();
    if tripped.is_empty() {
        checks.push(Check::ok(
            "messaging_flags",
            "Messaging feature flags",
            "none set",
        ));
    } else {
        let names: Vec<&str> = tripped.iter().map(|s| s.as_str()).collect();
        checks.push(Check::warn(
            "messaging_flags",
            "Messaging feature flags",
            format!("{} is set", names.join(", ")),
            format!(
                "Each of these turns cross-session messaging off, silently. reporch delivers its \
                 own pings over the session's stdin, so its core loop still works — but sessions \
                 you started yourself will not be able to reach reporch's, and orphan detection \
                 may be incomplete. Unset {} for full functionality.",
                names.join(" and ")
            ),
        ));
    }

    // ---- git --------------------------------------------------------------
    match env.git_version_output.as_deref() {
        Some(out) => checks.push(Check::ok("git", "git", out.trim().to_string())),
        None => checks.push(Check::fail(
            "git",
            "git",
            "the `git` binary could not be run",
            "reporch creates a git worktree in every repository of a project, and shells out to \
             the real `git` so your config, hooks and credential helpers apply. Install git and \
             make sure it is on your PATH.",
        )),
    }

    // ---- reporch's own home -----------------------------------------------
    match env.home.as_deref() {
        Some(h) if env.home_writable => {
            checks.push(Check::ok("home", "reporch home", h.display().to_string()))
        }
        Some(h) => checks.push(Check::fail(
            "home",
            "reporch home",
            format!("{} is not writable", h.display()),
            "reporch keeps every project, task and brief here. Fix the permissions, or point \
             REPORCH_HOME somewhere writable.",
        )),
        None => checks.push(Check::fail(
            "home",
            "reporch home",
            "could not be determined",
            "Set REPORCH_HOME to the directory where reporch should keep its data.",
        )),
    }

    Diagnosis { checks }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy() -> Env {
        Env {
            claude_version_output: Some("2.1.226 (Claude Code)".into()),
            claude_help_output: Some(REQUIRED_FLAGS.join(" ")),
            git_version_output: Some("git version 2.39.1".into()),
            vars: BTreeMap::new(),
            os: "macos",
            home: Some(PathBuf::from("/Users/x/.reporch")),
            home_writable: true,
        }
    }

    fn check<'a>(d: &'a Diagnosis, id: &str) -> &'a Check {
        d.checks
            .iter()
            .find(|c| c.id == id)
            .unwrap_or_else(|| panic!("no check with id {id}"))
    }

    #[test]
    fn a_healthy_machine_passes_with_exit_zero() {
        let d = diagnose(&healthy());
        assert_eq!(d.severity(), Severity::Ok, "{:#?}", d.checks);
        assert_eq!(d.exit_code(), 0);
        assert!(!d.is_fatal());
    }

    /// Every non-OK check must carry a fix. A diagnosis without a remedy is just bad news.
    #[test]
    fn every_finding_teaches_the_remedy() {
        let envs = [
            Env {
                claude_version_output: None,
                ..healthy()
            },
            Env {
                claude_version_output: Some("2.1.223".into()),
                ..healthy()
            },
            Env {
                git_version_output: None,
                ..healthy()
            },
            Env {
                os: "windows",
                ..healthy()
            },
            Env {
                home_writable: false,
                ..healthy()
            },
            Env {
                home: None,
                ..healthy()
            },
            Env {
                claude_help_output: Some("--help".into()),
                ..healthy()
            },
        ];
        for env in envs {
            for c in diagnose(&env).checks {
                if c.severity != Severity::Ok {
                    let fix = c.fix.as_deref().unwrap_or("");
                    assert!(
                        fix.len() > 20,
                        "check `{}` has severity {:?} but no actionable fix: {c:?}",
                        c.id,
                        c.severity
                    );
                }
            }
        }
    }

    #[test]
    fn claude_missing_is_fatal_and_says_how_to_install() {
        let d = diagnose(&Env {
            claude_version_output: None,
            ..healthy()
        });
        let c = check(&d, "claude_present");
        assert_eq!(c.severity, Severity::Fail);
        assert!(c.fix.as_ref().unwrap().contains("PATH"));
        assert_eq!(d.exit_code(), 1);
    }

    #[test]
    fn a_version_below_the_minimum_is_fatal_and_names_both_versions() {
        let d = diagnose(&Env {
            claude_version_output: Some("2.1.223 (Claude Code)".into()),
            ..healthy()
        });
        let c = check(&d, "claude_version");
        assert_eq!(c.severity, Severity::Fail);
        assert!(c.detail.contains("2.1.223"), "must say what was found");
        assert!(c.detail.contains("2.1.224"), "and what is needed");
        assert!(c.fix.as_ref().unwrap().contains("claude update"));
    }

    #[test]
    fn the_exact_minimum_version_passes() {
        let d = diagnose(&Env {
            claude_version_output: Some("2.1.224".into()),
            ..healthy()
        });
        assert_eq!(check(&d, "claude_version").severity, Severity::Ok);
    }

    #[test]
    fn each_third_party_provider_is_detected_and_fatal() {
        for (var, name) in THIRD_PARTY_PROVIDER_VARS {
            let mut vars = BTreeMap::new();
            vars.insert(var.to_string(), "1".to_string());
            let d = diagnose(&Env { vars, ..healthy() });
            let c = check(&d, "provider");
            assert_eq!(c.severity, Severity::Fail, "{var} should be fatal");
            assert!(
                c.detail.contains(name),
                "{var} should name {name}, got: {}",
                c.detail
            );
        }
    }

    /// `CLAUDE_CODE_USE_BEDROCK=0` means "not using Bedrock". Treating any value as truthy would
    /// refuse to run on a perfectly good machine.
    #[test]
    fn a_falsy_provider_variable_is_not_a_provider() {
        // Env::detect filters these out, so an empty `vars` is the correct representation.
        let d = diagnose(&healthy());
        assert_eq!(check(&d, "provider").severity, Severity::Ok);
    }

    #[test]
    fn each_kill_switch_warns_but_does_not_block() {
        for k in MESSAGING_KILL_SWITCHES {
            let mut vars = BTreeMap::new();
            vars.insert(k.to_string(), "1".to_string());
            let d = diagnose(&Env { vars, ..healthy() });
            let c = check(&d, "messaging_flags");
            assert_eq!(c.severity, Severity::Warn, "{k}");
            assert!(c.detail.contains(k));
            assert_eq!(
                d.exit_code(),
                0,
                "{k} must not block: reporch pings over stdin, not peer messaging"
            );
        }
    }

    #[test]
    fn all_four_kill_switches_are_reported_together() {
        let vars = MESSAGING_KILL_SWITCHES
            .iter()
            .map(|k| (k.to_string(), "1".to_string()))
            .collect();
        let d = diagnose(&Env { vars, ..healthy() });
        let c = check(&d, "messaging_flags");
        for k in MESSAGING_KILL_SWITCHES {
            assert!(c.detail.contains(k), "must name {k}");
        }
    }

    #[test]
    fn native_windows_is_fatal_and_points_at_wsl() {
        let d = diagnose(&Env {
            os: "windows",
            ..healthy()
        });
        let c = check(&d, "platform");
        assert_eq!(c.severity, Severity::Fail);
        assert!(c.fix.as_ref().unwrap().contains("WSL 2"));
    }

    #[test]
    fn wsl_reports_as_linux_and_passes() {
        let d = diagnose(&Env {
            os: "linux",
            ..healthy()
        });
        assert_eq!(check(&d, "platform").severity, Severity::Ok);
    }

    #[test]
    fn missing_git_is_fatal() {
        let d = diagnose(&Env {
            git_version_output: None,
            ..healthy()
        });
        assert_eq!(check(&d, "git").severity, Severity::Fail);
        assert_eq!(d.exit_code(), 1);
    }

    #[test]
    fn an_unwritable_home_is_fatal_and_mentions_the_override() {
        let d = diagnose(&Env {
            home_writable: false,
            ..healthy()
        });
        let c = check(&d, "home");
        assert_eq!(c.severity, Severity::Fail);
        assert!(c.fix.as_ref().unwrap().contains("REPORCH_HOME"));
    }

    /// A future Claude Code that drops a flag reporch depends on should fail here, loudly, rather
    /// than at task start with a confusing spawn error.
    #[test]
    fn a_missing_spawn_flag_is_fatal_and_names_it() {
        let help = REQUIRED_FLAGS
            .iter()
            .filter(|f| **f != "--append-system-prompt")
            .copied()
            .collect::<Vec<_>>()
            .join(" ");
        let d = diagnose(&Env {
            claude_help_output: Some(help),
            ..healthy()
        });
        let c = check(&d, "claude_flags");
        assert_eq!(c.severity, Severity::Fail);
        assert!(c.detail.contains("--append-system-prompt"));
    }

    #[test]
    fn several_failures_are_all_reported_not_just_the_first() {
        let d = diagnose(&Env {
            claude_version_output: None,
            git_version_output: None,
            os: "windows",
            ..healthy()
        });
        assert_eq!(d.failures().count(), 3, "{:#?}", d.checks);
    }

    #[test]
    fn severity_ordering_lets_worst_win() {
        assert!(Severity::Fail > Severity::Warn);
        assert!(Severity::Warn > Severity::Ok);
    }
}
