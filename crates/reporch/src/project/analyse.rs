//! Project analysis (§5.4): read every selected repo with Claude, then synthesise.
//!
//! One `claude -p --json-schema` call per repo, **in parallel**, so the output is structured
//! rather than prose. The two highest-value fields in the schema are `consumes` and
//! `produces_contracts_for`: they give reporch a directed graph over the project, which is then
//! reused three times — to elect the master (§5.7), to order the merge (§11.1), and to tell the
//! master which repos a contract change is likely to ripple into.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use reporch_core::{GraphNode, ProjectBase, ProjectRepo, RepoProfile};
use serde_json::json;

/// The §5.4 schema, passed to `--json-schema` so the model cannot answer in prose.
pub fn repo_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "role": { "type": "string", "description": "What this repo is, e.g. 'backend API server'" },
            "one_liner": { "type": "string", "description": "One sentence: what it does and what it owns" },
            "languages": { "type": "array", "items": { "type": "string" } },
            "frameworks": { "type": "array", "items": { "type": "string" } },
            "package_manager": { "type": "string" },
            "entrypoints": { "type": "array", "items": { "type": "string" } },
            "commands": {
                "type": "object",
                "description": "install / test / lint / run, as the project actually invokes them",
                "additionalProperties": { "type": "string" }
            },
            "deploy": {
                "type": "object",
                "properties": {
                    "target": { "type": "string" },
                    "how": { "type": "string" },
                    "url": { "type": "string" }
                }
            },
            "public_interfaces": { "type": "array", "items": { "type": "string" } },
            "consumes": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Names of OTHER REPOSITORIES IN THIS PROJECT whose contracts this repo depends on. Use the exact repository names given. Empty if none."
            },
            "produces_contracts_for": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Names of OTHER REPOSITORIES IN THIS PROJECT that depend on contracts this repo defines. Use the exact repository names given. Empty if none."
            },
            "env_files": { "type": "array", "items": { "type": "string" } },
            "untracked_needed_for_build": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Paths that are gitignored but required for the project to build or run, e.g. .env, node_modules, .venv"
            },
            "confidence": { "type": "string", "enum": ["high", "medium", "low"] }
        },
        "required": ["role", "one_liner", "consumes", "produces_contracts_for", "confidence"]
    })
}

/// The prompt for one repo.
///
/// The peer list is given explicitly and the model is told to use those exact names, because a
/// graph edge naming a repo that does not exist is an edge reporch has to throw away.
pub fn repo_prompt(repo: &str, peers: &[String]) -> String {
    format!(
        "\
Analyse this repository, `{repo}`, as one part of a larger multi-repository project.

The other repositories in this project are, exactly:
{peer_list}

Work out what this repository is for, how it is built and run, and — most importantly — how it
relates to those other repositories. For `consumes` and `produces_contracts_for`, use **only**
names from the list above, spelled exactly as written. A contract is any interface one repository
defines and another depends on: an HTTP API, a published package, a shared schema, a generated
client, an event payload.

Look at the actual code: imports, package manifests, API clients, generated types, environment
variables pointing at other services, and the README. Do not guess from the name alone. If you
cannot tell, return an empty list rather than a plausible one — a wrong edge is worse than a
missing one, because reporch uses this graph to decide merge order.

Be quick. Read what you need and answer.",
        peer_list = if peers.is_empty() {
            "(none — this is the only repository)".to_string()
        } else {
            peers
                .iter()
                .map(|p| format!("  - {p}"))
                .collect::<Vec<_>>()
                .join("\n")
        }
    )
}

/// The synthesis prompt (§5.4): one overview from all the per-repo profiles.
pub fn synthesis_prompt(project: &str, profiles: &BTreeMap<String, RepoProfile>) -> String {
    let mut s = format!(
        "These are the repositories of a project called `{project}`. Write the project overview.\n\n"
    );
    for (name, p) in profiles {
        s.push_str(&format!(
            "## {name}\n{}\nLanguages: {}\nConsumes: {}\nProduces contracts for: {}\n\n",
            p.one_liner,
            p.languages.join(", "),
            if p.consumes.is_empty() {
                "—".into()
            } else {
                p.consumes.join(", ")
            },
            if p.produces_contracts_for.is_empty() {
                "—".into()
            } else {
                p.produces_contracts_for.join(", ")
            },
        ));
    }
    s.push_str(
        "\nIn two or three sentences: what is this project, what does it do, and how do its parts \
         fit together? Write it for someone who has never seen it. No preamble, no bullet list — \
         just the paragraph.",
    );
    s
}

/// How one repo's analysis went.
#[derive(Debug, Clone)]
pub enum AnalysisOutcome {
    Ok(Box<RepoProfile>),
    /// Analysis failed. Not fatal: a project with one unreadable repo is still usable, and the
    /// user can re-run analysis from project settings (§5.5).
    Failed {
        repo: String,
        reason: String,
    },
}

/// Run `claude -p --json-schema` for one repo.
///
/// Budget-capped, because this runs once per repo and a project can have nine.
pub async fn analyse_repo(
    repo: &str,
    path: &Path,
    peers: &[String],
    model: &str,
    budget_usd: f64,
) -> AnalysisOutcome {
    let schema = repo_schema().to_string();
    let prompt = repo_prompt(repo, peers);

    let out = tokio::process::Command::new("claude")
        .args([
            "-p",
            "--output-format",
            "json",
            "--model",
            model,
            "--permission-mode",
            // Analysis only reads. It does not need, and must not have, write access.
            "plan",
            "--max-budget-usd",
            &format!("{budget_usd:.2}"),
            "--json-schema",
            &schema,
            // No MCP servers for a read-only analysis pass: they cost tokens and add surface.
            "--strict-mcp-config",
            &prompt,
        ])
        .current_dir(path)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await;

    let out = match out {
        Ok(o) => o,
        Err(e) => {
            return AnalysisOutcome::Failed {
                repo: repo.to_string(),
                reason: format!("could not run claude: {e}"),
            }
        }
    };
    if !out.status.success() {
        return AnalysisOutcome::Failed {
            repo: repo.to_string(),
            reason: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        };
    }

    match parse_profile(repo, &String::from_utf8_lossy(&out.stdout)) {
        Ok(p) => AnalysisOutcome::Ok(Box::new(p)),
        Err(e) => AnalysisOutcome::Failed {
            repo: repo.to_string(),
            reason: e,
        },
    }
}

/// Pull the profile out of `--output-format json`.
///
/// The envelope carries the structured answer in `result`, which may be an object or a JSON
/// string depending on how the schema was satisfied — both are accepted, because failing the
/// whole analysis over an envelope detail would be absurd.
pub fn parse_profile(repo: &str, stdout: &str) -> Result<RepoProfile, String> {
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .map_err(|e| format!("claude did not emit JSON: {e}"))?;

    let inner = v
        .get("structured_output")
        .or_else(|| v.get("result"))
        .cloned()
        .unwrap_or(v);
    let inner = match inner {
        serde_json::Value::String(s) => serde_json::from_str::<serde_json::Value>(&s)
            .map_err(|e| format!("the result was a string that is not JSON: {e}"))?,
        other => other,
    };

    let mut profile: RepoProfile = serde_json::from_value(inner)
        .map_err(|e| format!("the analysis did not match the schema: {e}"))?;
    profile.schema_version = 1;
    profile.name = repo.to_string();
    Ok(profile)
}

/// Assemble `project_base.json` from the per-repo profiles (§8.1).
///
/// Graph edges are kept only where **both** ends are repos in this project — enforced again in
/// `DepGraph::build`, which also reports what it dropped.
pub fn assemble(
    project: &str,
    repos: &[(String, PathBuf, Option<String>)],
    profiles: &BTreeMap<String, RepoProfile>,
    overview: String,
    created_at: String,
) -> ProjectBase {
    let repositories: Vec<ProjectRepo> = repos
        .iter()
        .map(|(name, path, remote)| ProjectRepo {
            name: name.clone(),
            path: path.display().to_string(),
            remote_url: remote.clone(),
            default_branch: reporch_git::default_branch(path).ok(),
            purpose: profiles
                .get(name)
                .map(|p| p.one_liner.clone())
                .unwrap_or_default(),
            profile_ref: Some(format!("repos/{name}.json")),
            cross_session_inbound: detect_inbound_setting(path),
        })
        .collect();

    let graph: BTreeMap<String, GraphNode> = profiles
        .iter()
        .map(|(name, p)| {
            (
                name.clone(),
                GraphNode {
                    consumes: p.consumes.clone(),
                    produces_contracts_for: p.produces_contracts_for.clone(),
                },
            )
        })
        .collect();

    ProjectBase {
        schema_version: reporch_core::PROJECT_SCHEMA_VERSION,
        name: project.to_string(),
        created_at,
        analyzed_at: Some(reporch_store::now_rfc3339()),
        overview,
        repositories,
        graph,
        settings: Default::default(),
        next_task_id: 1,
    }
}

/// Whether a repo's own settings would refuse inbound peer messages (§4.4 Deviation).
///
/// Advisory only — reporch pings over stdin, which this cannot block — but worth surfacing,
/// because a refusing session shows no visible change anywhere else.
fn detect_inbound_setting(repo: &Path) -> Option<String> {
    for rel in [".claude/settings.json", ".claude/settings.local.json"] {
        let Ok(text) = std::fs::read_to_string(repo.join(rel)) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        if let Some(s) = v.get("crossSessionInbound").and_then(|x| x.as_str()) {
            if s != "accept" {
                return Some(s.to_string());
            }
        }
    }
    None
}

/// The prose overview, `project_base.md` (§5.4).
pub fn render_markdown(base: &ProjectBase, profiles: &BTreeMap<String, RepoProfile>) -> String {
    let mut s = format!("# {}\n\n{}\n\n", base.name, base.overview);

    s.push_str("## Repositories\n\n");
    for r in &base.repositories {
        s.push_str(&format!("### `{}`\n\n{}\n\n", r.name, r.purpose));
        if let Some(p) = profiles.get(&r.name) {
            if !p.languages.is_empty() {
                s.push_str(&format!("- **Built with:** {}\n", p.languages.join(", ")));
            }
            if !p.frameworks.is_empty() {
                s.push_str(&format!("- **Frameworks:** {}\n", p.frameworks.join(", ")));
            }
            if let Some(pm) = &p.package_manager {
                s.push_str(&format!("- **Package manager:** {pm}\n"));
            }
            if !p.commands.is_empty() {
                let cmds: Vec<String> = p
                    .commands
                    .iter()
                    .map(|(k, v)| format!("`{k}`: `{v}`"))
                    .collect();
                s.push_str(&format!("- **Commands:** {}\n", cmds.join(" · ")));
            }
            if let Some(d) = &p.deploy {
                if let Some(t) = &d.target {
                    s.push_str(&format!(
                        "- **Deploys to:** {t}{}\n",
                        d.url
                            .as_ref()
                            .map(|u| format!(" — {u}"))
                            .unwrap_or_default()
                    ));
                }
            }
            if !p.untracked_needed_for_build.is_empty() {
                s.push_str(&format!(
                    "- **Needed to build but not tracked:** {}\n",
                    p.untracked_needed_for_build.join(", ")
                ));
            }
        }
        s.push_str(&format!("- **Path:** `{}`\n", r.path));
        if let Some(u) = &r.remote_url {
            s.push_str(&format!("- **Remote:** `{u}`\n"));
        }
        if let Some(inbound) = &r.cross_session_inbound {
            s.push_str(&format!(
                "- ⚠ This repository's settings set `crossSessionInbound: \"{inbound}\"`. \
                 reporch's own pings are unaffected (they go over stdin), but sessions you start \
                 yourself will not be able to message it.\n"
            ));
        }
        s.push('\n');
    }

    s.push_str("## How the parts depend on each other\n\n");
    let mut any = false;
    for (name, node) in &base.graph {
        if node.produces_contracts_for.is_empty() {
            continue;
        }
        any = true;
        s.push_str(&format!(
            "- `{name}` → {}\n",
            node.produces_contracts_for
                .iter()
                .map(|c| format!("`{c}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !any {
        s.push_str("_No contract dependencies were detected between these repositories._\n");
    }
    s.push_str(
        "\nAn arrow means the left repository defines a contract the right one depends on. \
         reporch uses this to elect a task's master repository, to order merges so producers \
         land before consumers, and to tell the master where a change is likely to ripple.\n",
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schema_requires_the_two_fields_the_graph_needs() {
        let s = repo_schema();
        let req: Vec<&str> = s["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(req.contains(&"consumes"));
        assert!(req.contains(&"produces_contracts_for"));
    }

    /// A graph edge naming a repo that does not exist is an edge reporch throws away, so the
    /// prompt must name the peers exactly.
    #[test]
    fn the_prompt_lists_the_peers_and_insists_on_exact_names() {
        let p = repo_prompt("modulex", &["modulex-ui".into(), "modulex-js".into()]);
        assert!(p.contains("- modulex-ui"));
        assert!(p.contains("- modulex-js"));
        assert!(p.contains("spelled exactly as written"));
        assert!(p.contains("empty list rather than a plausible one"));
    }

    #[test]
    fn a_single_repo_project_prompts_sensibly() {
        let p = repo_prompt("solo", &[]);
        assert!(p.contains("this is the only repository"));
    }

    #[test]
    fn parses_a_structured_output_envelope() {
        let stdout = r#"{"type":"result","subtype":"success","structured_output":
            {"role":"backend API server","one_liner":"REST API and auth.",
             "languages":["Python"],"consumes":[],"produces_contracts_for":["modulex-ui"],
             "confidence":"high"}}"#;
        let p = parse_profile("modulex", stdout).unwrap();
        assert_eq!(p.name, "modulex");
        assert_eq!(p.one_liner, "REST API and auth.");
        assert_eq!(p.produces_contracts_for, vec!["modulex-ui"]);
        assert_eq!(p.schema_version, 1);
    }

    #[test]
    fn parses_a_result_string_containing_json() {
        let stdout = r#"{"type":"result","result":"{\"role\":\"sdk\",\"one_liner\":\"JS SDK.\",\"consumes\":[\"modulex\"],\"produces_contracts_for\":[],\"confidence\":\"medium\"}"}"#;
        let p = parse_profile("modulex-js", stdout).unwrap();
        assert_eq!(p.consumes, vec!["modulex"]);
    }

    #[test]
    fn parses_a_bare_object() {
        let stdout = r#"{"role":"docs","one_liner":"Docs site.","consumes":["modulex"],
                         "produces_contracts_for":[],"confidence":"low"}"#;
        assert_eq!(
            parse_profile("modulex-docs", stdout).unwrap().consumes,
            vec!["modulex"]
        );
    }

    #[test]
    fn a_non_json_answer_is_an_error_naming_the_problem() {
        let e = parse_profile("x", "I had a look and it seems to be a backend.").unwrap_err();
        assert!(e.contains("did not emit JSON"), "{e}");
    }

    #[test]
    fn a_schema_mismatch_is_an_error() {
        let e = parse_profile("x", r#"{"structured_output":{"languages":"Python"}}"#).unwrap_err();
        assert!(e.contains("did not match the schema"), "{e}");
    }

    fn profiles() -> BTreeMap<String, RepoProfile> {
        let mut m = BTreeMap::new();
        m.insert(
            "modulex".to_string(),
            RepoProfile {
                schema_version: 1,
                name: "modulex".into(),
                one_liner: "REST API and auth; owns the database.".into(),
                languages: vec!["Python".into()],
                frameworks: vec!["FastAPI".into()],
                package_manager: Some("uv".into()),
                commands: [("test".to_string(), "pytest".to_string())]
                    .into_iter()
                    .collect(),
                produces_contracts_for: vec!["modulex-ui".into()],
                untracked_needed_for_build: vec![".env".into()],
                confidence: "high".into(),
                ..Default::default()
            },
        );
        m.insert(
            "modulex-ui".to_string(),
            RepoProfile {
                schema_version: 1,
                name: "modulex-ui".into(),
                one_liner: "Web app.".into(),
                consumes: vec!["modulex".into()],
                confidence: "high".into(),
                ..Default::default()
            },
        );
        m
    }

    #[test]
    fn assembling_builds_the_graph_from_the_profiles() {
        let d = tempfile::tempdir().unwrap();
        let repos = vec![
            ("modulex".to_string(), d.path().join("modulex"), None),
            ("modulex-ui".to_string(), d.path().join("modulex-ui"), None),
        ];
        let base = assemble(
            "modulex",
            &repos,
            &profiles(),
            "ModuleX is…".into(),
            "t".into(),
        );

        assert_eq!(base.name, "modulex");
        assert_eq!(base.repositories.len(), 2);
        assert_eq!(
            base.repo("modulex").unwrap().purpose,
            "REST API and auth; owns the database."
        );
        assert_eq!(
            base.graph["modulex"].produces_contracts_for,
            vec!["modulex-ui"]
        );
        assert_eq!(base.graph["modulex-ui"].consumes, vec!["modulex"]);
        assert!(base.analyzed_at.is_some());
        // Consent is never inherited from analysis (§12.1).
        assert!(!base.settings.has_consent());
    }

    /// The graph feeds master election and merge order, so it has to survive the assembly.
    #[test]
    fn the_assembled_graph_elects_the_backend_and_orders_the_merge() {
        let d = tempfile::tempdir().unwrap();
        let repos = vec![
            ("modulex".to_string(), d.path().join("modulex"), None),
            ("modulex-ui".to_string(), d.path().join("modulex-ui"), None),
        ];
        let base = assemble("modulex", &repos, &profiles(), String::new(), "t".into());
        let g = reporch_core::DepGraph::build(&base.repo_names(), &base.graph);

        assert_eq!(g.rank_upstream(&base.repo_names())[0], "modulex");
        assert_eq!(g.merge_order().order, vec!["modulex", "modulex-ui"]);
    }

    #[test]
    fn markdown_covers_the_repos_and_the_graph() {
        let d = tempfile::tempdir().unwrap();
        let repos = vec![
            (
                "modulex".to_string(),
                d.path().join("modulex"),
                Some("git@github.com:o/modulex.git".to_string()),
            ),
            ("modulex-ui".to_string(), d.path().join("modulex-ui"), None),
        ];
        let base = assemble(
            "modulex",
            &repos,
            &profiles(),
            "ModuleX is an agent platform.".into(),
            "t".into(),
        );
        let md = render_markdown(&base, &profiles());

        assert!(md.starts_with("# modulex"));
        assert!(md.contains("ModuleX is an agent platform."));
        assert!(md.contains("### `modulex`"));
        assert!(md.contains("FastAPI"));
        assert!(md.contains("`test`: `pytest`"));
        assert!(md.contains(".env"), "carry-over hints must be visible");
        assert!(md.contains("git@github.com:o/modulex.git"));
        assert!(md.contains("`modulex` → `modulex-ui`"));
    }

    #[test]
    fn markdown_says_so_when_there_is_no_graph() {
        let d = tempfile::tempdir().unwrap();
        let repos = vec![("solo".to_string(), d.path().join("solo"), None)];
        let base = assemble("solo", &repos, &BTreeMap::new(), String::new(), "t".into());
        let md = render_markdown(&base, &BTreeMap::new());
        assert!(md.contains("No contract dependencies were detected"));
    }

    /// §4.4 Deviation: a repo can refuse inbound peer messages, and it shows no sign of it
    /// anywhere else.
    #[test]
    fn a_repo_that_refuses_inbound_messages_is_detected_and_surfaced() {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path().join("modulex-ws");
        std::fs::create_dir_all(repo.join(".claude")).unwrap();
        std::fs::write(
            repo.join(".claude/settings.json"),
            r#"{"crossSessionInbound":"refuse"}"#,
        )
        .unwrap();
        assert_eq!(detect_inbound_setting(&repo).as_deref(), Some("refuse"));

        let base = assemble(
            "modulex",
            &[("modulex-ws".to_string(), repo, None)],
            &BTreeMap::new(),
            String::new(),
            "t".into(),
        );
        let md = render_markdown(&base, &BTreeMap::new());
        assert!(
            md.contains("crossSessionInbound"),
            "the warning must reach the user: {md}"
        );
        assert!(md.contains("stdin"), "and explain why it is not fatal");
    }

    #[test]
    fn accept_is_not_reported_as_a_problem() {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path().join("r");
        std::fs::create_dir_all(repo.join(".claude")).unwrap();
        std::fs::write(
            repo.join(".claude/settings.json"),
            r#"{"crossSessionInbound":"accept"}"#,
        )
        .unwrap();
        assert_eq!(detect_inbound_setting(&repo), None);
    }

    #[test]
    fn a_repo_with_no_claude_settings_is_fine() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(detect_inbound_setting(d.path()), None);
    }

    #[test]
    fn the_synthesis_prompt_carries_every_repo_and_its_edges() {
        let p = synthesis_prompt("modulex", &profiles());
        assert!(p.contains("## modulex"));
        assert!(p.contains("## modulex-ui"));
        assert!(p.contains("Produces contracts for: modulex-ui"));
        assert!(p.contains("Consumes: modulex"));
    }
}
