//! End-to-end validation: one real task across three real repositories with real Claude
//! sessions (Phase 4 of the build brief).
//!
//! **This spends real money.** Every session is capped with `--max-budget-usd`, every session is
//! terminated at the end, and the whole transcript is written out so the run can be read
//! afterwards. Run it deliberately:
//!
//! ```bash
//! cargo run --release -p reporch --example e2e -- <repos-dir> <out-dir> [budget-per-session]
//! ```
//!
//! It is an example rather than a test because `cargo test` must never spend money.

use std::collections::BTreeMap;
use std::path::PathBuf;

use reporch_core::{
    CarryMode, CarryOver, Effort, EffortPlan, GraphNode, ProjectBase, ProjectRepo, ProjectSettings,
};
use reporch_store::{Config, Paths};
use tokio::sync::mpsc;

use reporch::task::runtime::{StartedTask, TaskRunEvent, TaskRunRequest};
use reporch::task::{CreateRequest, RunView};

const REPOS: [&str; 3] = ["contracts-api", "contracts-client", "contracts-docs"];

/// A proof run gets a branch unique to its output directory. Reusing that directory produces the
/// same branch and exercises task resumption; a separate run cannot collide with a still-live
/// historical worktree from another harness invocation.
fn run_slug(home: &std::path::Path) -> String {
    // FNV-1a is deterministic across processes and Rust versions, unlike `DefaultHasher`.
    let hash = home
        .to_string_lossy()
        .as_bytes()
        .iter()
        .fold(0x811c_9dc5_u32, |hash, byte| {
            (hash ^ u32::from(*byte)).wrapping_mul(0x0100_0193)
        });
    format!("widget-version-field-{hash:08x}")
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let repos_dir = PathBuf::from(
        args.next()
            .expect("usage: e2e <repos-dir> <out-dir> [budget]"),
    );
    let home = PathBuf::from(
        args.next()
            .expect("usage: e2e <repos-dir> <out-dir> [budget]"),
    );
    let budget: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(2.00);

    // This harness is not the `reporch` binary, so it must say where that binary is — the
    // sessions have to be able to run it.
    if std::env::var_os("REPORCH_BIN").is_none() {
        if let Some(dir) = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().and_then(|d| d.parent()).map(|d| d.to_path_buf()))
        {
            std::env::set_var("REPORCH_BIN", dir);
        }
    }
    println!(
        "bin     {:?}",
        std::env::var("REPORCH_BIN").unwrap_or_default()
    );

    let paths = Paths::at(&home);
    let mut config = Config::init_home(&paths)?;
    config.session.default_model = "sonnet".into();
    config.session.max_budget_usd_per_session = budget;
    config.limits.task_timeout_secs = std::env::var("E2E_DEADLINE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(420);

    println!("── reporch end-to-end ──────────────────────────────────────");
    println!("repos   {}", repos_dir.display());
    println!("home    {}", home.display());
    println!(
        "budget  ${budget:.2} per session × {} = ${:.2} ceiling\n",
        REPOS.len(),
        budget * 3.0
    );

    // ---- the project ------------------------------------------------------
    // Hand-written rather than analysed, so this run measures the orchestration and not the
    // analysis pass. The graph is the true one: the API owns the Widget shape.
    let mut graph = BTreeMap::new();
    graph.insert(
        "contracts-api".to_string(),
        GraphNode {
            consumes: vec![],
            produces_contracts_for: vec!["contracts-client".into(), "contracts-docs".into()],
        },
    );
    let base = ProjectBase {
        schema_version: 1,
        name: "contracts".into(),
        created_at: reporch_store::now_rfc3339(),
        analyzed_at: Some(reporch_store::now_rfc3339()),
        overview: "A tiny three-repo project. contracts-api owns the Widget JSON shape; \
                   contracts-client consumes it; contracts-docs documents it."
            .into(),
        repositories: REPOS
            .iter()
            .map(|n| ProjectRepo {
                name: n.to_string(),
                path: repos_dir.join(n).display().to_string(),
                remote_url: None,
                default_branch: Some("main".into()),
                purpose: match *n {
                    "contracts-api" => "Backend; owns the Widget JSON shape in schema.json.",
                    "contracts-client" => "Consumes the Widget shape.",
                    _ => "Documents the Widget shape.",
                }
                .into(),
                profile_ref: None,
                cross_session_inbound: None,
            })
            .collect(),
        graph,
        settings: ProjectSettings {
            // §12.1 consent, given deliberately for this run.
            consented_at: Some(reporch_store::now_rfc3339()),
            default_model: "sonnet".into(),
            max_budget_usd_per_session: budget,
            worktree_carry_over: vec![CarryOver {
                pattern: ".env".into(),
                mode: CarryMode::Copy,
            }],
            ..Default::default()
        },
        next_task_id: 1,
    };
    std::fs::create_dir_all(paths.project_dir("contracts"))?;
    reporch_store::atomic::write_json(&paths.project_base_json("contracts"), &base)?;

    // ---- run the same actor as the console --------------------------------
    let brief = "Add a `version` field to the Widget object: a string, always \"1\".\n\n\
                 This changes the contract that contracts-api owns, so contracts-client and \
                 contracts-docs both have to follow.";
    let req = CreateRequest {
        project: "contracts".into(),
        brief: brief.into(),
        slug: run_slug(&home),
        model: "sonnet".into(),
        effort: EffortPlan::new(Effort::Medium, false),
        master: "contracts-api".into(),
        budget_usd_per_session: budget,
    };
    let request = TaskRunRequest {
        paths: paths.clone(),
        config,
        project: base,
        create: req,
    };
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let (_command_tx, command_rx) = mpsc::unbounded_channel();
    let actor = tokio::spawn(reporch::task::runtime::run_task(
        request, event_tx, command_rx,
    ));
    let mut started: Option<StartedTask> = None;
    let mut view = RunView::default();
    let mut seen_notices = 0usize;
    let mut last_activity = BTreeMap::<String, String>::new();
    let mut runtime_error = None;

    while let Some(event) = event_rx.recv().await {
        match event {
            TaskRunEvent::Preparing => println!("preparing worktrees…"),
            TaskRunEvent::Started(value) => {
                println!("task    {} on {}", value.task.id, value.task.branch);
                for repo in &value.task.repositories {
                    println!(
                        "  spawned {:<18} pid {:?} in {}",
                        repo.name, repo.session_pid, repo.worktree_path
                    );
                }
                for warning in &value.warnings {
                    println!("  warning: {warning}");
                }
                println!("→ kickoff delivered to {}\n", value.task.master_repo);
                started = Some(*value);
            }
            TaskRunEvent::View(next) => {
                for row in &next.rows {
                    if last_activity.get(&row.repo) != Some(&row.last) {
                        println!("  {:<18} {}", row.repo, row.last);
                        last_activity.insert(row.repo.clone(), row.last.clone());
                    }
                }
                for notice in next.notices.iter().skip(seen_notices) {
                    println!("  ‣ {notice}");
                }
                seen_notices = next.notices.len();
                view = next;
            }
            TaskRunEvent::Failed(error) => {
                runtime_error = Some(error);
                break;
            }
            TaskRunEvent::Stopped => break,
        }
    }
    actor.await?;
    if let Some(error) = runtime_error {
        return Err(error.into());
    }
    let started = started.ok_or("task actor stopped before sessions were started")?;

    // ---- what happened ----------------------------------------------------
    println!("\n── result ──────────────────────────────────────────────────");
    println!("spend    ${:.4} of ${:.2}", view.spent_usd, view.budget_usd);
    println!("\nREPO                 STATE      SPEND     BRIEFS");
    for r in &view.rows {
        println!(
            "{:<20} {:<10} ${:<8.4} {} in {} out",
            r.repo, r.state, r.spent_usd, r.briefs_in, r.briefs_out
        );
    }

    let idx = reporch_store::mailbox::load_index(&started.paths)?;
    println!(
        "\nMAILBOX  {} briefs, {} pings",
        idx.briefs().count(),
        idx.pings().count()
    );
    for b in idx.briefs() {
        println!(
            "  {} {} → {:<18} [{}] {}",
            b.id, b.from, b.to, b.status, b.subject
        );
    }
    for p in idx.pings() {
        println!("  ping → {:<18} by {:?} — {}", p.to, p.by, p.reason);
    }

    println!("\nDIFFS");
    for r in &started.task.repositories {
        let wt = PathBuf::from(&r.worktree_path);
        let changed = reporch_git::has_changes(&wt, &r.base_commit).unwrap_or(false);
        let stat = reporch_git::git_raw(&wt, &["diff", "--stat", "HEAD"])
            .map(|o| o.stdout.trim().to_string())
            .unwrap_or_default();
        let committed = reporch_git::git_raw(
            &wt,
            &["log", "--oneline", &format!("{}..HEAD", r.base_commit)],
        )
        .map(|o| o.stdout.trim().to_string())
        .unwrap_or_default();
        println!("  {:<20} changed={changed}", r.name);
        for line in stat.lines().chain(committed.lines()) {
            println!("      {line}");
        }
    }

    println!("\nartifacts in {}", started.paths.dir().display());
    println!("  task.json · base.md · protocol.md · prompt.md · briefs/ · logs/");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_directory_gives_each_proof_run_a_stable_unique_branch_slug() {
        let first = run_slug(std::path::Path::new("scratch/e2e-proof-a"));
        let again = run_slug(std::path::Path::new("scratch/e2e-proof-a"));
        let second = run_slug(std::path::Path::new("scratch/e2e-proof-b"));

        assert_eq!(first, again, "the same run remains resumable");
        assert_ne!(
            first, second,
            "independent proof runs must not share a branch"
        );
        assert!(first.starts_with("widget-version-field-"));
        assert!(first.len() <= 40, "branch slugs keep the §8 bound");
    }
}
