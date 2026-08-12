//! `reporch protocol` — print the rules (§7.2).
//!
//! *"The escape hatch when a session is unsure."* A session that has lost the thread of what it
//! may do can always run this and get the rules back, in full, without asking anyone.

use std::io::Write;

/// The protocol text. Also written to `<task>/protocol.md` at task start and injected with
/// `--append-system-prompt`, so all three are the same words (§7.3).
pub const PROTOCOL_TEXT: &str = include_str!("../../protocol.md");

pub fn run(json: bool) -> i32 {
    let out = std::io::stdout();
    let mut out = out.lock();
    if json {
        let _ = writeln!(
            out,
            "{}",
            serde_json::json!({
                "rules": {
                    "brief": "anyone may write a brief to anyone",
                    "ping_master": "the master may ping any worker",
                    "ping_worker": "a worker may ping nobody",
                    "ping_control_plane": "reporch pings on a worker's behalf, rate-limited and logged"
                },
                "text": PROTOCOL_TEXT,
            })
        );
    } else {
        let _ = write!(out, "{PROTOCOL_TEXT}");
    }
    crate::cli::exit::OK
}
