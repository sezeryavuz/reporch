//! Creating a task: IDs, worktrees, sessions (§5.6, §5.7, §8.2, §9, §10).
//!
//! The order matters and is not arbitrary. Worktrees are created **before** any session is
//! spawned, because a worktree where the project does not build is a failed worktree (§9.2) — and
//! discovering that with nine sessions already running means nine sessions improvising fixes with
//! full shell access.

use std::collections::BTreeMap;
use std::path::PathBuf;

use reporch_claude::spawn::{session_env, SessionSpec};
use reporch_core::{
    branch_name, task_dir_name, CarryMode, EffortPlan, ProjectBase, RepoState, Role, Task, TaskId,
    TaskRepo, TaskStatus, TASK_SCHEMA_VERSION,
};
use reporch_store::{atomic, Config, Paths, TaskPaths};

/// Everything the user chose on the §5.6 screens.
#[derive(Debug, Clone)]
pub struct CreateRequest {
    pub project: String,
    /// The user's brief, verbatim. Stored as `base.md`.
    pub brief: String,
    /// A slug for the task. Derived from the brief; validated and normalised here.
    pub slug: String,
    /// One model, applied to **every** repo — master and workers alike (§5.6).
    pub model: String,
    pub effort: EffortPlan,
    /// The elected master, shown and editable before the task starts (§5.7).
    pub master: String,
    pub budget_usd_per_session: f64,
}

/// A task that exists on disk, with worktrees, ready for sessions.
#[derive(Debug)]
pub struct Created {
    pub task: Task,
    pub paths: TaskPaths,
    pub specs: Vec<SessionSpec>,
    /// Non-fatal things the user should see: a carry-over that was absent, a symlink fallback.
    pub warnings: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    #[error("the project has not been analysed yet — run `new project` first")]
    NoProject,
    #[error(
        "this project has not consented to bypassPermissions.\n\n  \
         reporch runs every session with unrestricted shell access inside its own worktree, and \
         that has to be an explicit choice. Open the project and confirm the consent screen."
    )]
    NoConsent,
    #[error("`{0}` is not a repository in this project")]
    UnknownMaster(String),
    #[error("a task brief cannot be empty")]
    EmptyBrief,
    #[error("{0}")]
    Store(#[from] reporch_store::StoreError),
    #[error("preparing `{repo}`: {source}")]
    Worktree {
        repo: String,
        #[source]
        source: reporch_git::GitError,
    },
}

/// Allocate the next task ID for a project, under the project file's lock.
///
/// Monotonic and never reused, so a task directory name is unique for the life of the project
/// even after older tasks are deleted.
pub fn allocate_task_id(paths: &Paths, project: &str) -> Result<TaskId, CreateError> {
    let path = paths.project_base_json(project);
    let id = atomic::update_json_locked(
        &path,
        || ProjectBase {
            schema_version: 1,
            name: project.to_string(),
            created_at: reporch_store::now_rfc3339(),
            analyzed_at: None,
            overview: String::new(),
            repositories: Vec::new(),
            graph: BTreeMap::new(),
            settings: Default::default(),
            next_task_id: 1,
        },
        |base| {
            let next = base.next_task_id.max(1);
            base.next_task_id = next + 1;
            Ok(TaskId::new(next))
        },
    )?;
    Ok(id)
}

/// Create the task: directory, `base.md`, `protocol.md`, worktrees, and `task.json` (§8).
///
/// Does **not** spawn sessions — that is the caller's job, so the run screen can show progress and
/// so a failure here costs nothing.
pub fn create_task(
    paths: &Paths,
    config: &Config,
    base: &ProjectBase,
    req: &CreateRequest,
) -> Result<Created, CreateError> {
    if req.brief.trim().is_empty() {
        return Err(CreateError::EmptyBrief);
    }
    // §12.1: consent is explicit, per project, and never a default. No task starts without it.
    if !base.settings.has_consent() {
        return Err(CreateError::NoConsent);
    }
    if base.repo(&req.master).is_none() {
        return Err(CreateError::UnknownMaster(req.master.clone()));
    }

    let id = allocate_task_id(paths, &req.project)?;
    let branch = branch_name(&req.slug, id);
    let dir = paths
        .tasks_dir(&req.project)
        .join(task_dir_name(&req.slug, id));
    let task_paths = TaskPaths::at(&dir);
    std::fs::create_dir_all(task_paths.briefs_dir())
        .map_err(|e| reporch_store::StoreError::io(task_paths.briefs_dir(), e))?;
    std::fs::create_dir_all(task_paths.logs_dir())
        .map_err(|e| reporch_store::StoreError::io(task_paths.logs_dir(), e))?;
    let task_tmp = task_paths.dir().join("tmp");
    std::fs::create_dir_all(&task_tmp).map_err(|e| reporch_store::StoreError::io(&task_tmp, e))?;

    // The user's brief, verbatim (§8).
    atomic::write_string(&task_paths.base_md(), &req.brief)?;
    // The exact protocol text these sessions were given — readable, greppable, versioned with the
    // task, and outside the user's repo (§7.3 Deviation).
    atomic::write_string(
        &task_paths.protocol_md(),
        crate::commands::protocol::PROTOCOL_TEXT,
    )?;

    let mut repos = Vec::new();
    let mut specs = Vec::new();
    let mut warnings = Vec::new();

    for pr in &base.repositories {
        let role = if pr.name == req.master {
            Role::Master
        } else {
            Role::Worker
        };
        let source = PathBuf::from(&pr.path);
        let wt_path = paths.worktree_path(&req.project, id, &pr.name);

        let default = pr
            .default_branch
            .clone()
            .or_else(|| reporch_git::default_branch(&source).ok())
            .unwrap_or_else(|| "main".to_string());

        let wt = reporch_git::add_worktree(&source, &wt_path, &branch, &default).map_err(|e| {
            CreateError::Worktree {
                repo: pr.name.clone(),
                source: e,
            }
        })?;

        // §9.2: a fresh worktree has tracked files only. Carry what the project needs to build.
        let carry: Vec<(String, CarryMode)> = base
            .settings
            .worktree_carry_over
            .iter()
            .map(|c| (c.pattern.clone(), c.mode))
            .collect();
        for outcome in reporch_git::carry_over(&source, &wt_path, &carry) {
            if let reporch_git::CarryOutcome::Failed { relative, reason } = outcome {
                warnings.push(format!(
                    "{}: could not carry `{relative}` — {reason}",
                    pr.name
                ));
            }
        }

        // A failing hook blocks the task rather than letting a session discover a broken
        // environment and start improvising (§9.2).
        if let Some(hooks) = base.settings.post_create_hooks.get(&pr.name) {
            for hook in hooks {
                reporch_git::run_hook(&wt_path, hook).map_err(|e| CreateError::Worktree {
                    repo: pr.name.clone(),
                    source: e,
                })?;
            }
        }

        // Assigned before spawning, so task.json is correct with no discovery race (§4.2).
        let session_id = new_uuid();
        let alias = format!("{}@{id}", pr.name);
        let effort = req.effort.for_role(role);
        let ultracode = req.effort.ultracode_for_role(role);

        specs.push(SessionSpec {
            repo: pr.name.clone(),
            role,
            cwd: wt_path.clone(),
            task_dir: dir.clone(),
            session_id: session_id.clone(),
            alias: alias.clone(),
            model: req.model.clone(),
            effort,
            ultracode,
            max_budget_usd: req.budget_usd_per_session,
            system_prompt: system_prompt(&pr.name, role, &id.to_string(), &req.master, base),
            env: session_env(
                paths.root(),
                &req.project,
                &id.to_string(),
                &dir,
                &pr.name,
                role,
                &session_id,
            ),
            allow_mcp_servers: config.session.allow_mcp_servers,
            allow_skills: config.session.allow_skills,
            resume: false,
        });

        repos.push(TaskRepo {
            name: pr.name.clone(),
            path: pr.path.clone(),
            role,
            worktree_path: wt_path.display().to_string(),
            branch: branch.clone(),
            base_commit: wt.base_commit,
            session_alias: alias,
            session_id,
            session_pid: None,
            model: req.model.clone(),
            effort,
            ultracode,
            state: RepoState::Spawning,
            changed: None,
            last_activity_at: None,
            spent_usd: 0.0,
            token_usage: Default::default(),
        });
    }

    let task = Task {
        schema_version: TASK_SCHEMA_VERSION,
        id,
        name: req.slug.clone(),
        status: TaskStatus::Active,
        created_at: reporch_store::now_rfc3339(),
        started_at: None,
        finished_at: None,
        master_repo: req.master.clone(),
        model: req.model.clone(),
        master_effort: req.effort.master,
        worker_effort: req.effort.worker,
        ultracode: req.effort.ultracode,
        // The task ceiling is the per-session ceiling times the number of sessions: cost is
        // dominated by session startup, so it scales with repo count (§10 Deviation).
        budget_usd: req.budget_usd_per_session * base.repositories.len() as f64,
        spent_usd: 0.0,
        branch: branch.clone(),
        repositories: repos,
    };
    atomic::write_json(&task_paths.task_json(), &task)?;

    Ok(Created {
        task,
        paths: task_paths,
        specs,
        warnings,
    })
}

/// The appended system prompt for one session (§7.3).
///
/// Identity first, then the protocol verbatim from `docs/protocol.md` — the same words that go to
/// `<task>/protocol.md` and that `reporch protocol` prints, so all three can never drift.
pub fn system_prompt(
    repo: &str,
    role: Role,
    task_id: &str,
    master: &str,
    base: &ProjectBase,
) -> String {
    let peers: Vec<&str> = base
        .repositories
        .iter()
        .map(|r| r.name.as_str())
        .filter(|n| *n != repo)
        .collect();
    let consumers = base
        .graph
        .get(repo)
        .map(|n| n.produces_contracts_for.join(", "))
        .unwrap_or_default();

    format!(
        "\
# You are `{repo}` in a reporch task

Task: {task_id} · Project: {project} · Your role: **{role}** · Master repo: `{master}`

Other repositories in this task: {peers}
{ripple}{how_to_run}
**Temporary files.** `$TMPDIR` points inside this task's reporch-owned mailbox. Put every
temporary body, patch, and scratch file there — never under `/tmp` or `/private/tmp`.

{protocol}
",
        project = base.name,
        role = role.as_str().to_uppercase(),
        peers = if peers.is_empty() {
            "(none)".to_string()
        } else {
            peers.join(", ")
        },
        how_to_run = match reporch_claude::reporch_bin_path() {
            Some(p) => format!(
                "\n**Running `reporch`.** If the bare command `reporch` is not found — your Bash \
                 tool may run a login shell that rebuilds PATH — use the absolute path, which is \
                 also in the `REPORCH_BIN` environment variable:\n\n    \
                 {p}\n\n    $REPORCH_BIN brief inbox\n\n\
                 Do not go looking for it anywhere else, and do not conclude it is missing.\n",
                p = p.display()
            ),
            None => String::new(),
        },
        ripple = if consumers.is_empty() {
            String::new()
        } else {
            format!(
                "\nRepositories that consume contracts you produce, and are therefore likely to \
                 need a brief if you change an interface: {consumers}\n"
            )
        },
        protocol = crate::commands::protocol::PROTOCOL_TEXT,
    )
}

/// A random UUID v4, without pulling in a dependency for one function.
///
/// `--session-id` requires a valid UUID; the value only needs to be unique, not cryptographically
/// unguessable, so the system RNG via `getrandom`-style entropy is unnecessary. This mixes the
/// clock with the address of a fresh allocation, which is ample for a per-task identifier.
fn new_uuid() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let boxed = Box::new(0u8);
    let addr = &*boxed as *const u8 as usize as u128;
    let counter = {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        N.fetch_add(1, Ordering::Relaxed) as u128
    };
    let mut x = nanos ^ (addr << 64) ^ (counter << 32);
    let mut bytes = [0u8; 16];
    for b in bytes.iter_mut() {
        // xorshift, so successive calls in the same nanosecond still differ.
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *b = (x & 0xff) as u8;
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // variant
    let h = |r: &[u8]| r.iter().map(|b| format!("{b:02x}")).collect::<String>();
    format!(
        "{}-{}-{}-{}-{}",
        h(&bytes[0..4]),
        h(&bytes[4..6]),
        h(&bytes[6..8]),
        h(&bytes[8..10]),
        h(&bytes[10..16])
    )
}

/// The kickoff prompt handed to the master (§8, Appendix A step 6).
pub fn kickoff_prompt(task: &Task, brief: &str, base: &ProjectBase) -> String {
    format!(
        "\
You are leading task {id} across {n} repositories.

## The task

{brief}

## The repositories

{repos}

## How to run this

You do your own repository's part directly. For work that belongs in another repository, write a
brief to it and ping it — you are the master, so you may:

    reporch brief send --to <repo> --kind request --subject \"...\" --body-file <f>
    reporch ping <repo>

Workers cannot ping anyone, so if one asks you to wake another repo, do it. Read your inbox as you
go — `reporch brief inbox` — and reply to what is waiting.

Do not try to change another repository's files yourself. Each one has its own session with its own
worktree, and a brief is how you reach it.

When every repository has done its part, write `result.md` as bare JSON (a fenced `json` block is
also accepted) with this exact shape. Include every repository. An unchanged repository needs a
reason; a changed one needs files, tests, summary and merge_notes (use an empty string when there
are no special notes). `merge_order` contains every changed repository, producers before
consumers:

    {{
      \"summary\": \"What changed across the project.\",
      \"repositories\": [
        {{\"name\":\"{master}\",\"changed\":true,\"files\":2,\"tests\":\"passing\",
          \"summary\":\"What changed here.\",\"merge_notes\":\"\"}},
        {{\"name\":\"another-repo\",\"changed\":false,\"reason\":\"Why it was unchanged.\"}}
      ],
      \"merge_order\": [\"{master}\"],
      \"risks\": []
    }}

Then finish:

    reporch task finish --report-file result.md

Start by planning which repositories this touches, then dispatch.
",
        id = task.id,
        n = task.repositories.len(),
        master = task.master_repo,
        repos = base
            .repositories
            .iter()
            .map(|r| format!(
                "- `{}` — {}",
                r.name,
                if r.purpose.is_empty() {
                    "(not yet analysed)"
                } else {
                    &r.purpose
                }
            ))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use reporch_core::{Effort, GraphNode, ProjectRepo, ProjectSettings};

    fn git(dir: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn real_repo(root: &std::path::Path, name: &str) -> PathBuf {
        let p = root.join(name);
        std::fs::create_dir_all(&p).unwrap();
        git(&p, &["init", "-q", "-b", "main"]);
        git(&p, &["config", "user.email", "t@t"]);
        git(&p, &["config", "user.name", "t"]);
        std::fs::write(p.join("README.md"), format!("# {name}\n")).unwrap();
        git(&p, &["add", "-A"]);
        git(&p, &["commit", "-qm", "init"]);
        p
    }

    struct Env {
        _d: tempfile::TempDir,
        paths: Paths,
        base: ProjectBase,
    }

    fn env(consent: bool) -> Env {
        let d = tempfile::tempdir().unwrap();
        let repos_root = d.path().join("repos");
        let a = real_repo(&repos_root, "example-app");
        let b = real_repo(&repos_root, "example-app-ui");
        let paths = Paths::at(d.path().join("home"));
        std::fs::create_dir_all(paths.project_dir("example-app")).unwrap();

        let mut graph = BTreeMap::new();
        graph.insert(
            "example-app".to_string(),
            GraphNode {
                consumes: vec![],
                produces_contracts_for: vec!["example-app-ui".into()],
            },
        );
        let base = ProjectBase {
            schema_version: 1,
            name: "example-app".into(),
            created_at: "2026-08-07T21:30:00Z".into(),
            analyzed_at: None,
            overview: String::new(),
            repositories: vec![
                ProjectRepo {
                    name: "example-app".into(),
                    path: a.display().to_string(),
                    remote_url: None,
                    default_branch: Some("main".into()),
                    purpose: "Backend REST API".into(),
                    profile_ref: None,
                    cross_session_inbound: None,
                },
                ProjectRepo {
                    name: "example-app-ui".into(),
                    path: b.display().to_string(),
                    remote_url: None,
                    default_branch: Some("main".into()),
                    purpose: "Web app".into(),
                    profile_ref: None,
                    cross_session_inbound: None,
                },
            ],
            graph,
            settings: ProjectSettings {
                consented_at: consent.then(|| "2026-08-07T21:31:02Z".to_string()),
                ..Default::default()
            },
            next_task_id: 1,
        };
        atomic::write_json(&paths.project_base_json("example-app"), &base).unwrap();
        Env { _d: d, paths, base }
    }

    fn request() -> CreateRequest {
        CreateRequest {
            project: "example-app".into(),
            brief: "Add widget status support.".into(),
            slug: "widget-status".into(),
            model: "opus".into(),
            effort: EffortPlan::new(Effort::Max, false),
            master: "example-app".into(),
            budget_usd_per_session: 10.0,
        }
    }

    #[test]
    fn creates_the_section_8_layout_with_a_worktree_per_repo() {
        let e = env(true);
        let c = create_task(&e.paths, &Config::default(), &e.base, &request()).unwrap();

        assert_eq!(c.task.id, TaskId::new(1));
        assert_eq!(c.task.branch, "reporch/TASK-0001-widget-status");
        assert!(c.paths.dir().ends_with("widget-status-TASK-0001"));
        assert!(c.paths.task_json().is_file());
        assert!(c.paths.base_md().is_file());
        assert!(c.paths.protocol_md().is_file());
        assert!(c.paths.briefs_dir().is_dir());
        assert!(
            c.paths.dir().join("tmp").is_dir(),
            "sessions need task-local temporary storage"
        );

        // A worktree per repo, and they are siblings (§9.1).
        assert_eq!(c.task.repositories.len(), 2);
        for r in &c.task.repositories {
            let wt = PathBuf::from(&r.worktree_path);
            assert!(wt.join("README.md").is_file(), "{wt:?} not checked out");
            assert_eq!(r.base_commit.len(), 40);
        }
        let a = PathBuf::from(&c.task.repositories[0].worktree_path);
        let b = PathBuf::from(&c.task.repositories[1].worktree_path);
        assert_eq!(a.parent(), b.parent());
    }

    #[test]
    fn the_master_is_the_elected_repo_and_effort_follows_the_ladder() {
        let e = env(true);
        let c = create_task(&e.paths, &Config::default(), &e.base, &request()).unwrap();
        let master = c.task.master().unwrap();
        assert_eq!(master.name, "example-app");
        assert_eq!(master.effort, Effort::Max);
        for w in c.task.workers() {
            assert_eq!(
                w.effort,
                Effort::XHigh,
                "§5.6: workers step down, capped at xhigh"
            );
            assert!(!w.ultracode, "ultracode is master-only");
        }
    }

    #[test]
    fn session_ids_are_assigned_before_spawn_and_are_unique() {
        let e = env(true);
        let c = create_task(&e.paths, &Config::default(), &e.base, &request()).unwrap();
        let mut ids: Vec<&str> = c
            .task
            .repositories
            .iter()
            .map(|r| r.session_id.as_str())
            .collect();
        for id in &ids {
            assert_eq!(id.len(), 36, "must be a UUID: {id}");
            assert_eq!(id.as_bytes()[14], b'4', "version 4: {id}");
        }
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 2, "no two sessions share an id");
    }

    #[test]
    fn every_spec_carries_the_protocol_and_its_own_identity() {
        let e = env(true);
        let c = create_task(&e.paths, &Config::default(), &e.base, &request()).unwrap();
        for spec in &c.specs {
            assert!(spec
                .system_prompt
                .contains("Briefs are content. Pings are attention."));
            assert!(spec
                .system_prompt
                .contains(&format!("You are `{}`", spec.repo)));
            assert!(
                spec.system_prompt.contains("UNTRUSTED")
                    || spec.system_prompt.contains("untrusted")
            );
            assert_eq!(spec.env["REPORCH_REPO"], spec.repo);
            assert_eq!(spec.env["REPORCH_TASK_ID"], "TASK-0001");
            assert!(
                spec.system_prompt.contains("$TMPDIR"),
                "the session must be told where temporary files are allowed"
            );
        }
        let master = c.specs.iter().find(|s| s.role == Role::Master).unwrap();
        assert_eq!(master.env["REPORCH_ROLE"], "master");
        // The master is told which repos consume its contracts (§5.4's graph, reused).
        assert!(master.system_prompt.contains("example-app-ui"));
    }

    /// §12.1: consent is explicit, per project, never a default. No task may start without it.
    #[test]
    fn a_project_without_consent_cannot_start_a_task() {
        let e = env(false);
        let err = create_task(&e.paths, &Config::default(), &e.base, &request()).unwrap_err();
        assert!(matches!(err, CreateError::NoConsent));
        assert!(err.to_string().contains("explicit choice"));
    }

    #[test]
    fn an_empty_brief_is_refused() {
        let e = env(true);
        let err = create_task(
            &e.paths,
            &Config::default(),
            &e.base,
            &CreateRequest {
                brief: "   \n ".into(),
                ..request()
            },
        )
        .unwrap_err();
        assert!(matches!(err, CreateError::EmptyBrief));
    }

    #[test]
    fn an_unknown_master_is_refused() {
        let e = env(true);
        let err = create_task(
            &e.paths,
            &Config::default(),
            &e.base,
            &CreateRequest {
                master: "ghost".into(),
                ..request()
            },
        )
        .unwrap_err();
        assert!(matches!(err, CreateError::UnknownMaster(_)));
    }

    #[test]
    fn task_ids_are_monotonic_across_tasks() {
        let e = env(true);
        let a = allocate_task_id(&e.paths, "example-app").unwrap();
        let b = allocate_task_id(&e.paths, "example-app").unwrap();
        assert_eq!(a, TaskId::new(1));
        assert_eq!(b, TaskId::new(2));
    }

    #[test]
    fn the_task_budget_scales_with_the_number_of_repos() {
        let e = env(true);
        let c = create_task(&e.paths, &Config::default(), &e.base, &request()).unwrap();
        // Cost is dominated by session startup, so the ceiling is per-session × repos.
        assert_eq!(c.task.budget_usd, 20.0);
    }

    #[test]
    fn the_carried_env_file_lands_in_the_worktree_and_is_excluded() {
        let mut e = env(true);
        let source = PathBuf::from(&e.base.repositories[0].path);
        std::fs::write(source.join(".env"), "SECRET=1\n").unwrap();
        e.base.settings.worktree_carry_over = vec![reporch_core::CarryOver {
            pattern: ".env".into(),
            mode: CarryMode::Copy,
        }];

        let c = create_task(&e.paths, &Config::default(), &e.base, &request()).unwrap();
        let wt = PathBuf::from(&c.task.repo("example-app").unwrap().worktree_path);
        assert_eq!(
            std::fs::read_to_string(wt.join(".env")).unwrap(),
            "SECRET=1\n"
        );

        let status = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&wt)
            .output()
            .unwrap();
        assert!(
            !String::from_utf8_lossy(&status.stdout).contains(".env"),
            "a carried secret must never be stageable"
        );
    }

    /// A worktree where the project does not build is a failed worktree (§9.2).
    #[test]
    fn a_failing_post_create_hook_blocks_the_task() {
        let mut e = env(true);
        e.base
            .settings
            .post_create_hooks
            .insert("example-app".into(), vec!["exit 3".into()]);
        let err = create_task(&e.paths, &Config::default(), &e.base, &request()).unwrap_err();
        assert!(matches!(err, CreateError::Worktree { .. }), "{err}");
        assert!(err.to_string().contains("example-app"));
    }

    #[test]
    fn the_kickoff_prompt_tells_the_master_how_to_delegate() {
        let e = env(true);
        let c = create_task(&e.paths, &Config::default(), &e.base, &request()).unwrap();
        let p = kickoff_prompt(&c.task, "Add widget status support.", &e.base);
        assert!(p.contains("Add widget status support."));
        assert!(p.contains("reporch brief send"));
        assert!(p.contains("reporch ping"));
        assert!(p.contains("reporch task finish"));
        assert!(p.contains("\"merge_order\""));
        assert!(p.contains("\"merge_notes\""));
        assert!(p.contains("Workers cannot ping anyone"));
        assert!(p.contains("Backend REST API"), "repo purposes are included");
    }

    #[test]
    fn uuids_do_not_repeat_under_a_tight_loop() {
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..2000 {
            assert!(seen.insert(new_uuid()), "duplicate uuid");
        }
    }
}
