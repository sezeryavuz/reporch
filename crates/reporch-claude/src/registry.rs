//! Discovering live Claude Code sessions — used for **orphan detection only** (§4.1 Deviation).
//!
//! Spike #6 established what this can and cannot do:
//!
//! - `claude agents --json` **does** list app-spawned `-p` sessions, but reports **no `status`**
//!   for them and calls their `kind` `"interactive"`. So it cannot supply liveness, and it cannot
//!   distinguish reporch's children from the user's terminals.
//! - The registry file `~/.claude/sessions/<pid>.json` carries more — `entrypoint` (`"sdk-cli"`
//!   vs `"cli"`), `messagingSocketPath`, `version` — but is **undocumented**. It is read
//!   defensively, and nothing depends on it.
//!
//! Liveness comes from the event stream instead, which reporch owns. This module answers one
//! question: *after reporch crashed, which of its sessions are still running and spending money?*

use std::path::PathBuf;
use std::process::Command;

use serde::{Deserialize, Serialize};

/// A session observed on this machine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveSession {
    pub pid: u32,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(rename = "sessionId", default)]
    pub session_id: Option<String>,
    /// Absent for `-p` sessions (spike #6) — do not use it for liveness.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    /// Only in the registry file: `"sdk-cli"` for a `-p` session, `"cli"` for interactive.
    #[serde(default)]
    pub entrypoint: Option<String>,
}

impl LiveSession {
    /// Whether this looks like a session reporch spawned, by alias convention (§10: the alias is
    /// `<repo>@<TASK_ID>`).
    ///
    /// The alias is the only reliable marker: `kind` lies, and `entrypoint` is absent from the
    /// supported listing.
    pub fn looks_like_reporch(&self) -> bool {
        self.name.as_deref().is_some_and(|n| n.contains('@'))
    }

    /// Whether this session belongs to the given task.
    pub fn belongs_to_task(&self, task_id: &str) -> bool {
        self.name
            .as_deref()
            .is_some_and(|n| n.ends_with(&format!("@{task_id}")))
    }
}

/// List live sessions via the supported path, falling back to the registry files.
pub fn live_sessions() -> Vec<LiveSession> {
    match from_claude_agents() {
        Some(v) => v,
        None => from_registry_files(),
    }
}

/// `claude agents --json`. The supported path, preferred.
fn from_claude_agents() -> Option<Vec<LiveSession>> {
    // Argument order matters: `--add-dir` or `--mcp-config` placed *before* `agents` makes this
    // fail with `unknown option`. reporch passes neither, but the order is kept deliberate.
    let out = Command::new("claude")
        .args(["agents", "--json"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

/// `~/.claude/sessions/<pid>.json`. Undocumented; used only when the supported path fails.
fn from_registry_files() -> Vec<LiveSession> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    let dir = home.join(".claude/sessions");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| std::fs::read(e.path()).ok())
        .filter_map(|b| serde_json::from_slice::<LiveSession>(&b).ok())
        .collect()
}

/// Sessions from a given task that are still alive — the orphans a crashed reporch left behind,
/// still spending money.
pub fn orphans_for_task(task_id: &str) -> Vec<LiveSession> {
    live_sessions()
        .into_iter()
        .filter(|s| s.belongs_to_task(task_id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Vec<LiveSession> {
        serde_json::from_str(json).unwrap()
    }

    /// Verbatim shape from `claude agents --json` on 2.1.226 (spike #6).
    #[test]
    fn parses_the_real_agents_json_shape() {
        let v = parse(
            r#"[{"pid":26769,"cwd":"/x/spikerepo","kind":"interactive",
                 "startedAt":1786163651297,"sessionId":"996d96db","name":"reporch-spike-multi"}]"#,
        );
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].pid, 26769);
        assert_eq!(v[0].session_id.as_deref(), Some("996d96db"));
        assert_eq!(
            v[0].status, None,
            "spike #6: -p sessions report no status — never use it for liveness"
        );
    }

    #[test]
    fn parses_an_interactive_session_with_status() {
        let v = parse(
            r#"[{"pid":37645,"cwd":"/y","kind":"interactive","startedAt":1,
                 "sessionId":"e78","name":"modulex-33","status":"idle"}]"#,
        );
        assert_eq!(v[0].status.as_deref(), Some("idle"));
    }

    /// Verbatim registry-file shape, which has the extra fields.
    #[test]
    fn parses_the_registry_file_shape_including_entrypoint() {
        let s: LiveSession = serde_json::from_str(
            r#"{"pid":26769,"sessionId":"996d96db","cwd":"/x","startedAt":1,
                "procStart":"Sat","version":"2.1.226","peerProtocol":1,"kind":"interactive",
                "entrypoint":"sdk-cli","messagingSocketPath":"/tmp/cc-socks/26769.sock",
                "name":"reporch-spike-multi","updatedAt":2}"#,
        )
        .unwrap();
        assert_eq!(s.entrypoint.as_deref(), Some("sdk-cli"));
    }

    /// The registry format is undocumented and may change. Unknown fields must be ignored, and a
    /// missing field must default — never fail.
    #[test]
    fn unknown_and_missing_fields_are_tolerated() {
        let s: LiveSession =
            serde_json::from_str(r#"{"pid":1,"brand_new_field":{"a":[1,2]}}"#).unwrap();
        assert_eq!(s.pid, 1);
        assert_eq!(s.name, None);
        assert_eq!(s.cwd, "");
    }

    #[test]
    fn task_sessions_are_identified_by_their_alias() {
        let mine = LiveSession {
            pid: 1,
            cwd: String::new(),
            name: Some("modulex-ui@TASK-0001".into()),
            session_id: None,
            status: None,
            kind: None,
            entrypoint: None,
        };
        assert!(mine.looks_like_reporch());
        assert!(mine.belongs_to_task("TASK-0001"));
        assert!(!mine.belongs_to_task("TASK-0002"));

        let theirs = LiveSession {
            name: Some("modulex-33".into()),
            ..mine.clone()
        };
        assert!(
            !theirs.looks_like_reporch(),
            "the user's own sessions must never be treated as orphans"
        );
        assert!(!theirs.belongs_to_task("TASK-0001"));
    }

    /// A near-miss must not match: `TASK-0001` is not a prefix match for `TASK-00010`.
    #[test]
    fn task_matching_is_exact() {
        let s = LiveSession {
            pid: 1,
            cwd: String::new(),
            name: Some("ui@TASK-00010".into()),
            session_id: None,
            status: None,
            kind: None,
            entrypoint: None,
        };
        assert!(!s.belongs_to_task("TASK-0001"));
        assert!(s.belongs_to_task("TASK-00010"));
    }
}
