//! Building the session spawn command (§10).
//!
//! The command is constructed as data and only then executed, so the exact flag set is a snapshot
//! test rather than something you discover by reading a process listing. §10 fixes the flags; the
//! Phase 0 findings fix several of the details.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use reporch_core::{Effort, Role};
use serde_json::json;

/// Everything needed to launch one repo's session for one task.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSpec {
    pub repo: String,
    pub role: Role,
    /// The worktree. This is the process's working directory.
    pub cwd: PathBuf,
    /// The task mailbox, granted with `--add-dir` (§7.1).
    pub task_dir: PathBuf,
    /// Assigned by reporch **before** spawning, so `task.json` is correct with no discovery race.
    pub session_id: String,
    /// `modulex-ui@TASK-0001`.
    pub alias: String,
    pub model: String,
    pub effort: Effort,
    pub ultracode: bool,
    pub max_budget_usd: f64,
    /// The protocol text, injected with `--append-system-prompt` so **no file inside the user's
    /// repository is modified** (§7.3).
    pub system_prompt: String,
    /// `REPORCH_*` for the agent CLI (§7.1).
    pub env: BTreeMap<String, String>,
    /// Whether the operator's MCP servers may load. Default off (D006).
    pub allow_mcp_servers: bool,
    /// Whether the operator's skills and slash commands may load. Default on (D006).
    pub allow_skills: bool,
    /// Resuming an existing session after a crash. Flags are **not** restored by `--resume`, so
    /// the whole set is passed again either way (§10 Deviation).
    pub resume: bool,
}

/// A ready-to-execute command: program, argv, working directory and environment.
#[derive(Debug, Clone, PartialEq)]
pub struct SpawnCommand {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
}

impl SpawnCommand {
    /// A shell-ish rendering for logs and snapshots. Not for execution — reporch never goes
    /// through a shell, so nothing here needs to be shell-safe, only readable.
    pub fn to_display_string(&self) -> String {
        let mut out = String::from(&self.program);
        for a in &self.args {
            out.push(' ');
            if a.contains(' ') || a.contains('"') || a.contains('{') {
                out.push('\'');
                out.push_str(a);
                out.push('\'');
            } else {
                out.push_str(a);
            }
        }
        out
    }
}

/// The `--settings` payload (§10).
///
/// Two keys are load-bearing:
/// - `crossSessionInbound: "accept"` (§4.4). Note the §4.4 Deviation: a repo's own settings can
///   override this if they are *stricter*, and reporch pings over stdin regardless — so this is
///   defence in depth, not the mechanism.
/// - `permissions.deny` naming the two tools by **bare name**, which removes them from the
///   model's context entirely (spike #8).
pub fn settings_payload(spec: &SessionSpec) -> serde_json::Value {
    let mut deny: Vec<String> = crate::DENIED_TOOLS.iter().map(|s| s.to_string()).collect();
    // §12.2's deny rules. Scoped rules block the matching call; the tool stays available.
    deny.extend(
        [
            "Bash(git push:*)",
            "Bash(gh pr create:*)",
            "Bash(gh pr merge:*)",
            "Bash(curl:* | sh)",
            "Bash(curl:* | bash)",
            "Read(~/.ssh/**)",
            "Edit(~/.ssh/**)",
            "Write(~/.ssh/**)",
            "Read(~/.claude/**)",
            "Edit(~/.claude/**)",
            "Write(~/.claude/**)",
            "Edit(~/.reporch/config.toml)",
            "Write(~/.reporch/config.toml)",
            // Keep direct file-tool scratch writes inside the task mailbox. `--add-dir` is an
            // additive grant, not a filesystem sandbox (§12.2 Deviation, D015).
            "Edit(/tmp/**)",
            "Write(/tmp/**)",
            "Edit(/private/tmp/**)",
            "Write(/private/tmp/**)",
        ]
        .iter()
        .map(|s| s.to_string()),
    );

    let mut settings = json!({
        "crossSessionInbound": "accept",
        "permissions": { "deny": deny },
    });
    if spec.ultracode {
        // §4.3 Deviation / D007: `--effort ultracode` would pin effort to xhigh, so "max +
        // ultracode" is only expressible as the effort flag plus this key. The key is documented
        // as settable *only* via --settings, which is exactly the channel reporch uses.
        settings["ultracode"] = json!(true);
    }
    settings
}

/// Build the spawn command for one session (§10).
pub fn build(spec: &SessionSpec) -> SpawnCommand {
    let mut args: Vec<String> = vec![
        "-p".into(),
        "--input-format".into(),
        "stream-json".into(),
        "--output-format".into(),
        "stream-json".into(),
        // Required: `--output-format stream-json` refuses to run without it.
        "--verbose".into(),
        "--include-partial-messages".into(),
        // The delivery acknowledgement for an injected ping: the echo carries isReplay: true.
        "--replay-user-messages".into(),
    ];

    if spec.resume {
        args.push("--resume".into());
        args.push(spec.session_id.clone());
    } else {
        args.push("--session-id".into());
        args.push(spec.session_id.clone());
    }

    args.push("--name".into());
    args.push(spec.alias.clone());
    args.push("--model".into());
    args.push(spec.model.clone());
    args.push("--effort".into());
    args.push(spec.effort.as_str().into());
    args.push("--permission-mode".into());
    args.push("bypassPermissions".into());
    args.push("--settings".into());
    args.push(settings_payload(spec).to_string());
    args.push("--append-system-prompt".into());
    args.push(spec.system_prompt.clone());
    args.push("--add-dir".into());
    args.push(spec.task_dir.display().to_string());
    args.push("--max-budget-usd".into());
    args.push(format!("{:.2}", spec.max_budget_usd));

    if !spec.allow_mcp_servers {
        args.push("--strict-mcp-config".into());
    }
    if !spec.allow_skills {
        args.push("--disable-slash-commands".into());
    }

    SpawnCommand {
        program: "claude".into(),
        args,
        cwd: spec.cwd.clone(),
        env: spec.env.clone(),
    }
}

/// Prepend reporch's own directory to `PATH`, so a session can actually run `reporch`.
///
/// **Found by the first real end-to-end run**, and it broke the entire product: the sessions were
/// told to run `reporch brief read 0001`, got `command not found`, and — correctly — refused to
/// act on a brief they could not verify. The whole agent surface of §7 is unreachable if the
/// binary is not on the session's `PATH`.
///
/// reporch must not assume it was installed: it is routinely run from `target/release/` during
/// development, and from a Homebrew cellar or `~/.cargo/bin` otherwise. Prepending the running
/// executable's own directory works in every case and costs nothing when it is already there.
fn path_with_reporch() -> String {
    let existing = std::env::var("PATH").unwrap_or_default();
    let Some(dir) = reporch_bin_dir() else {
        return existing;
    };
    let dir = dir.display().to_string();
    if existing.split(':').any(|p| p == dir) {
        return existing;
    }
    if existing.is_empty() {
        dir
    } else {
        format!("{dir}:{existing}")
    }
}

/// Locate the directory containing the `reporch` binary.
///
/// Deliberately does not assume the running executable *is* `reporch`: reporch is also driven as
/// a library — by its own integration harness, and by anything embedding it — and in that case
/// `current_exe()` points at something else entirely. Getting this wrong is silent and total,
/// because the sessions then cannot run any `reporch` command at all.
///
/// In order: an explicit `REPORCH_BIN` override, the running executable's own directory, and its
/// parent (which covers `target/release/examples/` → `target/release/`).
fn reporch_bin_dir() -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var_os("REPORCH_BIN") {
        let p = std::path::PathBuf::from(p);
        let dir = if p.is_dir() {
            p
        } else {
            p.parent()?.to_path_buf()
        };
        if has_reporch(&dir) {
            return Some(dir);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    if has_reporch(dir) {
        return Some(dir.to_path_buf());
    }
    let up = dir.parent()?;
    has_reporch(up).then(|| up.to_path_buf())
}

fn has_reporch(dir: &std::path::Path) -> bool {
    let p = dir.join("reporch");
    std::fs::metadata(&p).map(|m| m.is_file()).unwrap_or(false)
}

/// The `REPORCH_*` environment injected into every session (§7.1).
///
/// Every agent-CLI subcommand resolves its context from these, so a session never passes a task
/// ID or a path. `REPORCH_ROLE` is what `reporch ping` checks to refuse a worker (§6.2).
pub fn session_env(
    home: &Path,
    project: &str,
    task_id: &str,
    task_dir: &Path,
    repo: &str,
    role: Role,
    session_id: &str,
) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    m.insert("REPORCH_HOME".into(), home.display().to_string());
    m.insert("REPORCH_PROJECT".into(), project.to_string());
    m.insert("REPORCH_TASK_ID".into(), task_id.to_string());
    m.insert("REPORCH_TASK_DIR".into(), task_dir.display().to_string());
    m.insert("REPORCH_REPO".into(), repo.to_string());
    m.insert("REPORCH_ROLE".into(), role.as_str().to_string());
    m.insert("REPORCH_SESSION_ID".into(), session_id.to_string());
    m.insert("TMPDIR".into(), task_dir.join("tmp").display().to_string());

    // Two mechanisms, because one of them is not reliable.
    //
    // `PATH` is the ergonomic one — a session can just type `reporch`. But Claude Code's Bash
    // tool runs a **login shell**, which re-sources the user's profile and rebuilds `PATH` from
    // scratch, discarding whatever the parent process set. Verified in the third end-to-end run:
    // the injected entry was simply absent, and the shell's `PATH` began with a duplicated
    // `~/.local/bin`, the signature of a profile being re-sourced.
    //
    // `REPORCH_BIN` is the reliable one. Ordinary environment variables are not rebuilt by a
    // profile, so an absolute path in a variable survives where a `PATH` entry does not. The
    // injected system prompt names it explicitly, so a session that finds `reporch` missing has
    // somewhere to go instead of hunting the filesystem.
    m.insert("PATH".into(), path_with_reporch());
    if let Some(bin) = reporch_bin_path() {
        m.insert("REPORCH_BIN".into(), bin.display().to_string());
    }
    m
}

/// The absolute path to the `reporch` binary, if it can be found.
pub fn reporch_bin_path() -> Option<std::path::PathBuf> {
    reporch_bin_dir().map(|d| d.join("reporch"))
}

/// A single injected user turn, as one NDJSON line (§6.4).
///
/// Delivering a ping is a line written to the session's stdin. No socket, no undocumented wire
/// format, and it cannot be held pending an approval dialog.
pub fn injection_line(text: &str) -> String {
    json!({
        "type": "user",
        "message": { "role": "user", "content": [{ "type": "text", "text": text }] }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> SessionSpec {
        SessionSpec {
            repo: "modulex-ui".into(),
            role: Role::Worker,
            cwd: PathBuf::from("/home/u/.reporch/worktrees/modulex/TASK-0001/modulex-ui"),
            task_dir: PathBuf::from(
                "/home/u/.reporch/projects/modulex/tasks/org-scoped-api-keys-TASK-0001",
            ),
            session_id: "1f4e8a20-0000-4000-8000-000000000001".into(),
            alias: "modulex-ui@TASK-0001".into(),
            model: "opus".into(),
            effort: Effort::XHigh,
            ultracode: false,
            max_budget_usd: 10.0,
            system_prompt: "PROTOCOL".into(),
            env: session_env(
                Path::new("/home/u/.reporch"),
                "modulex",
                "TASK-0001",
                Path::new("/home/u/.reporch/projects/modulex/tasks/org-scoped-api-keys-TASK-0001"),
                "modulex-ui",
                Role::Worker,
                "1f4e8a20-0000-4000-8000-000000000001",
            ),
            allow_mcp_servers: false,
            allow_skills: true,
            resume: false,
        }
    }

    fn arg_after<'a>(cmd: &'a SpawnCommand, flag: &str) -> Option<&'a str> {
        let i = cmd.args.iter().position(|a| a == flag)?;
        cmd.args.get(i + 1).map(String::as_str)
    }

    #[test]
    fn the_flag_set_matches_section_10() {
        let cmd = build(&spec());
        for flag in [
            "-p",
            "--input-format",
            "--output-format",
            "--verbose",
            "--include-partial-messages",
            "--replay-user-messages",
            "--session-id",
            "--name",
            "--model",
            "--effort",
            "--permission-mode",
            "--settings",
            "--append-system-prompt",
            "--add-dir",
            "--max-budget-usd",
        ] {
            assert!(cmd.args.iter().any(|a| a == flag), "missing {flag}");
        }
        assert_eq!(arg_after(&cmd, "--input-format"), Some("stream-json"));
        assert_eq!(arg_after(&cmd, "--output-format"), Some("stream-json"));
        assert_eq!(
            arg_after(&cmd, "--permission-mode"),
            Some("bypassPermissions")
        );
        assert_eq!(arg_after(&cmd, "--effort"), Some("xhigh"));
        assert_eq!(arg_after(&cmd, "--max-budget-usd"), Some("10.00"));
    }

    /// `--output-format stream-json` fails outright without `--verbose`. Losing this flag would
    /// break every spawn, so it is asserted separately from the list above.
    #[test]
    fn verbose_is_present_because_stream_json_requires_it() {
        assert!(build(&spec()).args.iter().any(|a| a == "--verbose"));
    }

    /// **Never `--bare`** (§4.5 Addition): it unbinds the inbox socket and hides the session from
    /// the agent list, which would silently remove orphan detection and all interop.
    #[test]
    fn never_bare() {
        assert!(
            !build(&spec()).args.iter().any(|a| a == "--bare"),
            "--bare unbinds the inbox socket and hides the session"
        );
    }

    #[test]
    fn the_session_id_is_assigned_before_spawn_not_discovered() {
        let cmd = build(&spec());
        assert_eq!(
            arg_after(&cmd, "--session-id"),
            Some("1f4e8a20-0000-4000-8000-000000000001")
        );
        assert!(!cmd.args.iter().any(|a| a == "--resume"));
    }

    #[test]
    fn resuming_swaps_session_id_for_resume_but_keeps_every_other_flag() {
        let cmd = build(&SessionSpec {
            resume: true,
            ..spec()
        });
        assert_eq!(
            arg_after(&cmd, "--resume"),
            Some("1f4e8a20-0000-4000-8000-000000000001")
        );
        assert!(!cmd.args.iter().any(|a| a == "--session-id"));
        // §10 Deviation: --resume restores the transcript, not the flags. All of these must be
        // re-passed or the resumed session loses the protocol and its mailbox.
        for flag in [
            "--settings",
            "--append-system-prompt",
            "--add-dir",
            "--model",
        ] {
            assert!(cmd.args.iter().any(|a| a == flag), "resume dropped {flag}");
        }
    }

    #[test]
    fn settings_deny_the_two_tools_by_bare_name() {
        let s = settings_payload(&spec());
        let deny = s["permissions"]["deny"].as_array().unwrap();
        let names: Vec<&str> = deny.iter().map(|d| d.as_str().unwrap()).collect();
        // Bare name = removed from context. A scoped rule would only block the call.
        assert!(names.contains(&"SendMessage"));
        assert!(names.contains(&"ListAgents"));
    }

    #[test]
    fn settings_accept_inbound_messages() {
        let s = settings_payload(&spec());
        assert_eq!(
            s["crossSessionInbound"], "accept",
            "§4.4: a bypassing receiver otherwise holds every inbound message forever"
        );
    }

    #[test]
    fn settings_deny_push_and_pr_creation() {
        let s = settings_payload(&spec());
        let names: Vec<String> = s["permissions"]["deny"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d.as_str().unwrap().to_string())
            .collect();
        let joined = names.join(" ");
        assert!(joined.contains("git push"), "§12.2: no push");
        assert!(joined.contains("gh pr create"), "§12.2: no PR creation");
        assert!(joined.contains(".ssh"), "§12.2: no ssh key access");
        assert!(joined.contains(".claude"), "§12.2: no writes to ~/.claude");
    }

    #[test]
    fn settings_keep_direct_file_tools_out_of_system_temp_directories() {
        let s = settings_payload(&spec());
        let deny: Vec<&str> = s["permissions"]["deny"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect();
        for rule in [
            "Edit(/tmp/**)",
            "Write(/tmp/**)",
            "Edit(/private/tmp/**)",
            "Write(/private/tmp/**)",
        ] {
            assert!(deny.contains(&rule), "missing {rule}: {deny:?}");
        }
    }

    #[test]
    fn session_temporary_files_stay_in_the_task_mailbox() {
        let task = Path::new("/h/projects/p/tasks/x-TASK-0001");
        let env = session_env(
            Path::new("/h"),
            "p",
            "TASK-0001",
            task,
            "repo",
            Role::Worker,
            "session",
        );
        assert_eq!(
            env.get("TMPDIR").map(String::as_str),
            Some("/h/projects/p/tasks/x-TASK-0001/tmp")
        );
    }

    /// D007: "max + ultracode" cannot be expressed as `--effort ultracode`, which pins effort to
    /// xhigh. It is the max effort flag plus the settings key.
    #[test]
    fn ultracode_is_a_settings_key_not_an_effort_value() {
        let cmd = build(&SessionSpec {
            effort: Effort::Max,
            ultracode: true,
            ..spec()
        });
        assert_eq!(arg_after(&cmd, "--effort"), Some("max"));
        assert!(!cmd.args.iter().any(|a| a == "ultracode"));
        let payload: serde_json::Value =
            serde_json::from_str(arg_after(&cmd, "--settings").unwrap()).unwrap();
        assert_eq!(payload["ultracode"], true);
    }

    #[test]
    fn ultracode_is_absent_when_off() {
        let payload = settings_payload(&spec());
        assert!(payload.get("ultracode").is_none());
    }

    #[test]
    fn mcp_servers_are_off_by_default_and_skills_are_on() {
        let cmd = build(&spec());
        assert!(cmd.args.iter().any(|a| a == "--strict-mcp-config"), "D006");
        assert!(
            !cmd.args.iter().any(|a| a == "--disable-slash-commands"),
            "skills stay available inside the user's own repos"
        );
    }

    #[test]
    fn both_can_be_re_enabled() {
        let cmd = build(&SessionSpec {
            allow_mcp_servers: true,
            allow_skills: false,
            ..spec()
        });
        assert!(!cmd.args.iter().any(|a| a == "--strict-mcp-config"));
        assert!(cmd.args.iter().any(|a| a == "--disable-slash-commands"));
    }

    #[test]
    fn add_dir_grants_the_mailbox_and_nothing_else() {
        let cmd = build(&spec());
        let dirs: Vec<&String> = cmd
            .args
            .iter()
            .enumerate()
            .filter(|(i, _)| i > &0 && cmd.args[i - 1] == "--add-dir")
            .map(|(_, a)| a)
            .collect();
        assert_eq!(dirs.len(), 1, "exactly one --add-dir");
        assert!(dirs[0].ends_with("org-scoped-api-keys-TASK-0001"));
    }

    #[test]
    fn the_working_directory_is_the_worktree_never_the_users_repo() {
        let cmd = build(&spec());
        assert!(cmd.cwd.to_string_lossy().contains("/worktrees/"));
    }

    /// **Regression, from the first real end-to-end run.** Both workers ran
    /// `reporch brief read 0001`, got `command not found`, and refused to act — correctly, but
    /// the task was dead. Every session must be able to reach the binary that is orchestrating
    /// it.
    /// These mutate the process environment, so they must not run concurrently with each other.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// **Regression, from the first real end-to-end run.** Both workers ran
    /// `reporch brief read 0001`, got `command not found`, and refused to act — correctly, but
    /// the task was dead. Every session must be able to reach the binary orchestrating it.
    #[test]
    fn a_findable_reporch_lands_on_the_session_path() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("reporch"), b"#!/bin/sh\n").unwrap();
        std::env::set_var("REPORCH_BIN", d.path());
        let env = session_env(
            Path::new("/h"),
            "p",
            "TASK-0001",
            Path::new("/t"),
            "r",
            Role::Worker,
            "s",
        );
        std::env::remove_var("REPORCH_BIN");

        let path = env.get("PATH").expect("PATH must always be injected");
        let found = path
            .split(':')
            .any(|p| std::fs::metadata(std::path::Path::new(p).join("reporch")).is_ok());
        assert!(
            found,
            "no directory on the session's PATH contains a `reporch` binary, so the whole agent \
             surface of §7 would be unreachable.\n  PATH: {path}"
        );
    }

    #[test]
    fn the_existing_path_is_preserved_not_replaced() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let path = path_with_reporch();
        let existing = std::env::var("PATH").unwrap_or_default();
        for entry in existing.split(':').filter(|p| !p.is_empty()) {
            assert!(path.contains(entry), "dropped {entry} from PATH");
        }
    }

    #[test]
    fn a_directory_without_the_binary_is_not_used() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("REPORCH_BIN", d.path());
        let path = path_with_reporch();
        std::env::remove_var("REPORCH_BIN");
        assert!(
            !path.starts_with(&d.path().display().to_string()),
            "an empty directory must not be prepended: {path}"
        );
    }

    #[test]
    fn path_is_injected_even_when_reporch_cannot_be_located() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = session_env(
            Path::new("/h"),
            "p",
            "T",
            Path::new("/t"),
            "r",
            Role::Worker,
            "s",
        );
        // Never absent: dropping PATH entirely would break every shell command a session runs.
        assert!(env.contains_key("PATH"));
    }

    #[test]
    fn env_matches_section_7_1() {
        let e = spec().env;
        assert_eq!(e["REPORCH_ROLE"], "worker");
        assert_eq!(e["REPORCH_REPO"], "modulex-ui");
        assert_eq!(e["REPORCH_PROJECT"], "modulex");
        assert_eq!(e["REPORCH_TASK_ID"], "TASK-0001");
        assert!(e.contains_key("REPORCH_TASK_DIR"));
        assert!(e.contains_key("REPORCH_SESSION_ID"));
        assert!(e.contains_key("REPORCH_HOME"));
    }

    #[test]
    fn the_master_gets_the_master_role() {
        let e = session_env(
            Path::new("/h"),
            "p",
            "TASK-0001",
            Path::new("/t"),
            "modulex",
            Role::Master,
            "s",
        );
        assert_eq!(e["REPORCH_ROLE"], "master");
    }

    #[test]
    fn an_injection_is_one_ndjson_line() {
        let line = injection_line("Brief 0003 is waiting.");
        assert!(!line.contains('\n'), "must be a single line");
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["type"], "user");
        assert_eq!(v["message"]["role"], "user");
        assert_eq!(v["message"]["content"][0]["text"], "Brief 0003 is waiting.");
    }

    #[test]
    fn an_injection_escapes_newlines_and_quotes() {
        let line = injection_line("line one\nline \"two\"");
        assert!(!line.contains('\n'));
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["message"]["content"][0]["text"], "line one\nline \"two\"");
    }

    /// The whole command is snapshotted: the flag set is the contract with Claude Code, and every
    /// change to it should be reviewed rather than noticed later (CLAUDE.md).
    #[test]
    fn spawn_command_snapshot() {
        insta::assert_snapshot!(build(&spec()).to_display_string());
    }

    #[test]
    fn master_with_ultracode_snapshot() {
        insta::assert_snapshot!(build(&SessionSpec {
            repo: "modulex".into(),
            role: Role::Master,
            alias: "modulex@TASK-0001".into(),
            effort: Effort::Max,
            ultracode: true,
            ..spec()
        })
        .to_display_string());
    }
}
