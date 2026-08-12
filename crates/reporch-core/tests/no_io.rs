//! `reporch-core` must stay pure (CLAUDE.md, §13.1).
//!
//! > *"`reporch-core` must be testable without spawning Claude, and without touching the
//! > filesystem. The protocol state machine is the part that will have subtle bugs, and it must be
//! > exercisable in milliseconds."*
//!
//! A convention that is only written down erodes. This test makes it a build failure: if anyone
//! adds a dependency to `reporch-core` that can touch the filesystem, the network, a clock, or a
//! process, the test names it and fails.
//!
//! The test itself runs `cargo metadata`, which is I/O — that is fine. The constraint is on the
//! *crate*, not on its test suite.

use std::collections::BTreeSet;
use std::process::Command;

/// The complete set of crates `reporch-core` is allowed to pull in, transitively.
///
/// Adding to this list is a deliberate act. Before you do, ask whether the thing you are adding
/// belongs in `reporch-store`, `reporch-claude` or `reporch-git` instead — it almost always does.
const ALLOWED: &[&str] = &[
    "reporch-core",
    // serialisation: pure data, no I/O
    "serde",
    "serde_derive",
    "serde_core",
    // error plumbing
    "thiserror",
    "thiserror-impl",
    // proc-macro machinery, compile-time only
    "proc-macro2",
    "quote",
    "syn",
    "unicode-ident",
];

/// Crates that would definitively break purity. Listed separately so the failure message can say
/// *why* rather than just "not allowed" — the point is to teach the boundary, not just enforce it.
fn why_forbidden(name: &str) -> Option<&'static str> {
    let reason = match name {
        "tokio" | "async-std" | "smol" => {
            "an async runtime — the state machine must be synchronous and instant"
        }
        "std-fs" | "fs4" | "tempfile" | "ignore" | "walkdir" | "notify" => "filesystem access",
        "reqwest" | "ureq" | "hyper" | "curl" => "network access",
        "chrono" | "time" | "jiff" => {
            "a clock — events carry `at_ms` so tests can be exact rather than flaky"
        }
        "rayon" => "a thread pool",
        "git2" | "libgit2-sys" => "git",
        "ratatui" | "crossterm" => "the terminal",
        "tracing" => "logging, which belongs at the I/O boundary",
        _ => return None,
    };
    Some(reason)
}

fn metadata() -> serde_json::Value {
    let out = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--format-version",
            "1",
            "--filter-platform",
            current_platform(),
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("`cargo metadata` must run");
    assert!(
        out.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("cargo metadata must emit JSON")
}

fn current_platform() -> &'static str {
    // Only used to prune platform-specific dependencies from the graph.
    if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "aarch64-apple-darwin"
        } else {
            "x86_64-apple-darwin"
        }
    } else if cfg!(target_arch = "aarch64") {
        "aarch64-unknown-linux-gnu"
    } else {
        "x86_64-unknown-linux-gnu"
    }
}

/// Walk the resolved dependency graph from `reporch-core`, following **normal** dependencies only.
/// Dev-dependencies are excluded on purpose: the test suite may use `proptest`, `insta` and
/// `serde_json` freely, because none of them ship in the library.
fn transitive_normal_deps(md: &serde_json::Value) -> BTreeSet<String> {
    let nodes = md["resolve"]["nodes"].as_array().expect("resolve.nodes");
    let packages = md["packages"].as_array().expect("packages");

    let id_to_name = |id: &str| -> String {
        packages
            .iter()
            .find(|p| p["id"].as_str() == Some(id))
            .and_then(|p| p["name"].as_str())
            .unwrap_or(id)
            .to_string()
    };

    let root_id = packages
        .iter()
        .find(|p| p["name"].as_str() == Some("reporch-core"))
        .and_then(|p| p["id"].as_str())
        .expect("reporch-core must be in the metadata")
        .to_string();

    let mut seen = BTreeSet::new();
    let mut stack = vec![root_id];
    while let Some(id) = stack.pop() {
        let Some(node) = nodes.iter().find(|n| n["id"].as_str() == Some(&id)) else {
            continue;
        };
        for dep in node["deps"].as_array().into_iter().flatten() {
            let kinds = dep["dep_kinds"].as_array();
            // `kind: null` is a normal dependency; "dev" and "build" are not shipped.
            let is_normal = kinds.is_some_and(|ks| ks.iter().any(|k| k["kind"].is_null()));
            if !is_normal {
                continue;
            }
            let Some(dep_id) = dep["pkg"].as_str() else {
                continue;
            };
            let name = id_to_name(dep_id);
            if seen.insert(name) {
                stack.push(dep_id.to_string());
            }
        }
    }
    seen
}

#[test]
fn reporch_core_has_no_io_dependencies() {
    let md = metadata();
    let deps = transitive_normal_deps(&md);
    let allowed: BTreeSet<&str> = ALLOWED.iter().copied().collect();

    let mut problems = Vec::new();
    for dep in &deps {
        if allowed.contains(dep.as_str()) {
            continue;
        }
        let reason = why_forbidden(dep)
            .map(|r| format!("it brings in {r}"))
            .unwrap_or_else(|| "it is not on the allow-list in tests/no_io.rs".to_string());
        problems.push(format!("  - `{dep}`: {reason}"));
    }

    assert!(
        problems.is_empty(),
        "reporch-core must perform no I/O (CLAUDE.md, §13.1), but its dependency tree now \
         contains:\n{}\n\nThe protocol state machine has to be exercisable in milliseconds \
         against a recorded event log, with no Claude process and no filesystem. If what you \
         need genuinely belongs here, add it to ALLOWED in this file — deliberately. Otherwise \
         it belongs in reporch-store, reporch-claude or reporch-git.\n\nFull tree: {deps:?}",
        problems.join("\n")
    );
}

/// The allow-list must not rot: an entry that no longer appears in the tree should be removed,
/// so the list keeps describing reality.
#[test]
fn the_allow_list_has_no_stale_entries() {
    let md = metadata();
    let mut deps = transitive_normal_deps(&md);
    deps.insert("reporch-core".to_string());
    let stale: Vec<&str> = ALLOWED
        .iter()
        .copied()
        .filter(|a| !deps.contains(*a))
        .collect();
    assert!(
        stale.is_empty(),
        "these entries in ALLOWED are no longer dependencies and should be deleted: {stale:?}"
    );
}
