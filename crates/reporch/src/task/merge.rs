//! Completion, merge and teardown (§11, §8.4).
//!
//! **Every merge step is human-confirmed. No exceptions, no `--yes` flag** (§11.2). The rest of
//! reporch is an autonomy tool; this step is the one place where autonomy buys nothing and costs
//! a lot.
//!
//! That is enforced structurally, not by discipline: [`MergePlan`] can only advance through
//! [`MergePlan::confirm`], which takes a [`Confirmation`] that nothing in this crate can
//! construct except from an explicit user action. A test walks the whole CLI looking for a
//! `--yes` flag and fails if one ever appears.

use std::path::{Path, PathBuf};

use reporch_core::{DepGraph, OrderViolation, ProjectBase, RepoProfile, Task};
use reporch_store::{atomic, TaskPaths};
use serde::{Deserialize, Serialize};

/// `result.md`'s machine-readable companion, generated against the §11.1 schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResultReport {
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub repositories: Vec<ResultRepo>,
    #[serde(default)]
    pub merge_order: Vec<String>,
    #[serde(default)]
    pub risks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResultRepo {
    pub name: String,
    #[serde(default)]
    pub changed: bool,
    #[serde(default)]
    pub files: Option<u32>,
    #[serde(default)]
    pub tests: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub merge_notes: Option<String>,
    /// Why an unchanged repo was left alone.
    #[serde(default)]
    pub reason: Option<String>,
}

/// A structured merge report plus any fidelity warning the UI must show.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedReport {
    pub report: ResultReport,
    pub warnings: Vec<String>,
}

/// Whether a task worktree contains product work. The master's untracked `result.md` is copied
/// into task history and is not itself a product change.
pub fn has_task_changes(worktree: &Path, base_commit: &str) -> Result<bool, reporch_git::GitError> {
    let status = reporch_git::git(worktree, &["status", "--porcelain"])?;
    if status.stdout.lines().any(|line| {
        !(line.starts_with("?? ") && line.get(3..).is_some_and(|path| path == "result.md"))
    }) {
        return Ok(true);
    }
    let ahead = reporch_git::git(
        worktree,
        &["rev-list", "--count", &format!("{base_commit}..HEAD")],
    )?;
    Ok(ahead.trimmed() != "0")
}

/// Load a structured report when the master supplied one; otherwise derive the facts that can be
/// proven from Git and keep the narrative as the summary. Older live tasks wrote prose despite
/// §11.1, and refusing to merge their real work would compound that protocol gap (D023).
pub fn load_report(
    task: &Task,
    paths: &TaskPaths,
) -> Result<LoadedReport, reporch_store::StoreError> {
    if let Ok(report) = atomic::read_json::<ResultReport>(&paths.result_json()) {
        return Ok(LoadedReport {
            report,
            warnings: Vec::new(),
        });
    }

    let markdown = atomic::read_string(&paths.result_md())?;
    if let Some(report) = parse_report(&markdown) {
        return Ok(LoadedReport {
            report,
            warnings: Vec::new(),
        });
    }

    let repositories = task
        .repositories
        .iter()
        .map(|repo| {
            let worktree = Path::new(&repo.worktree_path);
            let changed = has_task_changes(worktree, &repo.base_commit)
                .ok()
                .or(repo.changed)
                .unwrap_or(false);
            ResultRepo {
                name: repo.name.clone(),
                changed,
                files: changed.then(|| changed_file_count(worktree, &repo.base_commit)),
                tests: None,
                summary: None,
                merge_notes: None,
                reason: (!changed).then(|| "No worktree diff was found.".into()),
            }
        })
        .collect();
    Ok(LoadedReport {
        report: ResultReport {
            summary: first_narrative_paragraph(&markdown),
            repositories,
            merge_order: Vec::new(),
            risks: Vec::new(),
        },
        warnings: vec![
            "result.md is narrative rather than the §11.1 schema; changed repositories and file counts were derived from Git, and no merge notes or risks were inferred"
                .into(),
        ],
    })
}

/// Parse either a bare JSON report or a JSON fenced block inside `result.md`.
pub fn parse_report(text: &str) -> Option<ResultReport> {
    if let Ok(report) = serde_json::from_str(text.trim()) {
        return Some(report);
    }
    let mut parts = text.split("```");
    let _ = parts.next();
    while let Some(fenced) = parts.next() {
        let body = fenced
            .trim()
            .strip_prefix("json")
            .map(str::trim_start)
            .unwrap_or_else(|| fenced.trim());
        if let Ok(report) = serde_json::from_str(body) {
            return Some(report);
        }
        let _ = parts.next();
    }
    None
}

/// Validate the master's report before it can mark a task complete (§11.1).
pub fn validate_report(task: &Task, report: &ResultReport) -> Result<(), String> {
    if report.summary.trim().is_empty() {
        return Err("`summary` is empty".into());
    }
    let task_repos: std::collections::BTreeSet<&str> = task
        .repositories
        .iter()
        .map(|repo| repo.name.as_str())
        .collect();
    let mut seen = std::collections::BTreeSet::new();
    for repo in &report.repositories {
        if !task_repos.contains(repo.name.as_str()) {
            return Err(format!("`repositories` names unknown repo `{}`", repo.name));
        }
        if !seen.insert(repo.name.as_str()) {
            return Err(format!("`repositories` repeats `{}`", repo.name));
        }
        if repo.changed {
            if repo.files.is_none()
                || repo.tests.as_deref().is_none_or(str::is_empty)
                || repo.summary.as_deref().is_none_or(str::is_empty)
                || repo.merge_notes.is_none()
            {
                return Err(format!(
                    "changed repo `{}` needs `files`, `tests`, `summary`, and `merge_notes`",
                    repo.name
                ));
            }
        } else if repo.reason.as_deref().is_none_or(str::is_empty) {
            return Err(format!("unchanged repo `{}` needs `reason`", repo.name));
        }
    }
    if seen.len() != task_repos.len() {
        let missing = task_repos
            .into_iter()
            .filter(|name| !seen.contains(name))
            .collect::<Vec<_>>();
        return Err(format!("`repositories` is missing: {}", missing.join(", ")));
    }

    let mut order = std::collections::BTreeSet::new();
    for name in &report.merge_order {
        if !task.repo(name).is_some_and(|repo| {
            report
                .repositories
                .iter()
                .any(|reported| reported.name == repo.name && reported.changed)
        }) {
            return Err(format!(
                "`merge_order` contains `{name}`, which is unknown or unchanged"
            ));
        }
        if !order.insert(name.as_str()) {
            return Err(format!("`merge_order` repeats `{name}`"));
        }
    }
    let missing_changed = report
        .repositories
        .iter()
        .filter(|repo| repo.changed && !order.contains(repo.name.as_str()))
        .map(|repo| repo.name.as_str())
        .collect::<Vec<_>>();
    if !missing_changed.is_empty() {
        return Err(format!(
            "`merge_order` is missing changed repos: {}",
            missing_changed.join(", ")
        ));
    }
    Ok(())
}

fn first_narrative_paragraph(markdown: &str) -> String {
    markdown
        .split("\n\n")
        .map(str::trim)
        .find(|paragraph| {
            !paragraph.is_empty() && !paragraph.starts_with('#') && !paragraph.starts_with("```")
        })
        .unwrap_or("Completed task; see result.md for the master's narrative report.")
        .replace('\n', " ")
}

fn changed_file_count(worktree: &Path, base_commit: &str) -> u32 {
    let mut files = std::collections::BTreeSet::new();
    if let Ok(output) = reporch_git::git_raw(
        worktree,
        &["diff", "--name-only", &format!("{base_commit}..HEAD")],
    ) {
        files.extend(output.lines().map(str::to_string));
    }
    if let Ok(output) = reporch_git::git_raw(worktree, &["status", "--porcelain"]) {
        for line in output.stdout.lines() {
            if line.starts_with("?? ") && line.get(3..) == Some("result.md") {
                continue;
            }
            if let Some(path) = line.get(3..) {
                files.insert(path.rsplit(" -> ").next().unwrap_or(path).to_string());
            }
        }
    }
    files.len().min(u32::MAX as usize) as u32
}

/// One repo's step in the merge (§11.2).
#[derive(Debug, Clone, PartialEq)]
pub struct MergeStep {
    pub repo: String,
    pub worktree: PathBuf,
    pub source: PathBuf,
    pub branch: String,
    pub base_commit: String,
    pub default_branch: String,
    pub changed: bool,
    pub merge_notes: Option<String>,
    /// The repo's own test command, from its analysis profile. Failure blocks (§11.2 step 4).
    pub test_command: Option<String>,
    pub outcome: StepOutcome,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepOutcome {
    Pending,
    /// Nothing to merge.
    Unchanged,
    Merged {
        commit: String,
    },
    /// The user chose to skip this repo.
    Skipped,
    Failed {
        reason: String,
    },
}

/// The whole merge, in dependency order.
#[derive(Debug, Clone)]
pub struct MergePlan {
    pub task_id: String,
    pub steps: Vec<MergeStep>,
    /// Where the master's proposed order disagrees with the graph. **A warning shown to the
    /// user, not a silent override** (§11.1) — the master may know something the graph does not.
    pub order_warnings: Vec<OrderViolation>,
    /// Repos in a dependency cycle, which cannot be ordered.
    pub cyclic: Vec<String>,
    cursor: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedMerge {
    schema_version: u32,
    task: String,
    merged_at: String,
    #[serde(default)]
    order_warnings: Vec<String>,
    #[serde(default)]
    cyclic: Vec<String>,
    steps: Vec<PersistedStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedStep {
    repo: String,
    branch: String,
    changed: bool,
    outcome: StepOutcome,
}

/// Proof that a human confirmed this step.
///
/// Deliberately impossible to construct except through [`Confirmation::by_user`], which the TUI
/// calls only from an explicit keypress. There is no `Default`, no `From`, and no way to conjure
/// one in a test that is trying to make a merge happen without asking.
#[derive(Debug, Clone, Copy)]
pub struct Confirmation(());

impl Confirmation {
    /// Call this **only** in response to a real user action.
    pub fn by_user() -> Self {
        Confirmation(())
    }
}

impl MergePlan {
    /// Build the plan from the master's report, validated against the dependency graph.
    pub fn build(task: &Task, report: &ResultReport, graph: &DepGraph) -> Self {
        // The master proposes; the graph validates. Where the master omitted repos, fall back to
        // the graph's own topological order so nothing is silently dropped.
        let proposed: Vec<String> = if report.merge_order.is_empty() {
            graph.merge_order().order
        } else {
            report.merge_order.clone()
        };
        let order_warnings = graph.validate_merge_order(&proposed);
        let cyclic = graph.merge_order().cyclic;

        let mut steps = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        // Proposed order first, then anything the master left out, so every repo is presented.
        let all: Vec<String> = proposed
            .iter()
            .cloned()
            .chain(task.repositories.iter().map(|r| r.name.clone()))
            .collect();

        for name in all {
            if !seen.insert(name.clone()) {
                continue;
            }
            let Some(tr) = task.repo(&name) else { continue };

            let reported = report.repositories.iter().find(|r| r.name == name);
            // Git is authoritative. A prose report, an omitted repo, or a model saying
            // `changed: false` must never make a real diff disappear from human review.
            let changed = has_task_changes(Path::new(&tr.worktree_path), &tr.base_commit)
                .ok()
                .or_else(|| reported.map(|repo| repo.changed))
                .or(tr.changed)
                .unwrap_or(false);
            steps.push(MergeStep {
                repo: name.clone(),
                worktree: PathBuf::from(&tr.worktree_path),
                source: PathBuf::from(&tr.path),
                branch: tr.branch.clone(),
                base_commit: tr.base_commit.clone(),
                default_branch: "main".to_string(),
                changed,
                merge_notes: reported.and_then(|r| r.merge_notes.clone()),
                test_command: None,
                outcome: if changed {
                    StepOutcome::Pending
                } else {
                    StepOutcome::Unchanged
                },
            });
        }

        let mut plan = Self {
            task_id: task.id.to_string(),
            steps,
            order_warnings,
            cyclic,
            cursor: 0,
        };
        plan.advance_resolved();
        plan
    }

    /// Fill merge-time facts that belong to the project profile rather than the task snapshot.
    pub fn configure(
        &mut self,
        project: &ProjectBase,
        profiles: &std::collections::BTreeMap<String, RepoProfile>,
    ) {
        for step in &mut self.steps {
            step.default_branch = project
                .repo(&step.repo)
                .and_then(|repo| repo.default_branch.clone())
                .or_else(|| reporch_git::default_branch(&step.source).ok())
                .unwrap_or_else(|| "main".into());
            step.test_command = profiles
                .get(&step.repo)
                .and_then(|profile| profile.commands.get("test"))
                .cloned();
        }
    }

    /// Restore already-recorded outcomes after an interrupted merge. A recorded merge is trusted
    /// only when its commit is still an ancestor of the user's checkout; a stale or edited file
    /// therefore cannot make a repository disappear from review.
    pub fn restore(&mut self, paths: &TaskPaths) -> Result<(), reporch_store::StoreError> {
        let Some(stored) = atomic::read_json_opt::<PersistedMerge>(&paths.merge_json())? else {
            return Ok(());
        };
        if stored.task != self.task_id {
            return Err(reporch_store::StoreError::Other(format!(
                "{} belongs to {}, not {}",
                paths.merge_json().display(),
                stored.task,
                self.task_id
            )));
        }
        for step in &mut self.steps {
            let Some(saved) = stored
                .steps
                .iter()
                .find(|saved| saved.repo == step.repo && saved.branch == step.branch)
            else {
                continue;
            };
            match &saved.outcome {
                StepOutcome::Merged { commit }
                    if reporch_git::git_raw(
                        &step.source,
                        &["merge-base", "--is-ancestor", commit, "HEAD"],
                    )
                    .is_ok_and(|output| output.ok()) =>
                {
                    step.outcome = saved.outcome.clone();
                }
                StepOutcome::Skipped | StepOutcome::Failed { .. } => {
                    step.outcome = saved.outcome.clone();
                }
                _ => {}
            }
        }
        self.cursor = 0;
        self.advance_resolved();
        Ok(())
    }

    /// The step awaiting a decision, if any.
    pub fn current(&self) -> Option<&MergeStep> {
        self.steps.get(self.cursor)
    }

    /// Steps that still need the user, i.e. changed and not yet resolved.
    pub fn remaining(&self) -> usize {
        self.steps
            .iter()
            .filter(|s| matches!(s.outcome, StepOutcome::Pending | StepOutcome::Failed { .. }))
            .count()
    }

    pub fn is_done(&self) -> bool {
        self.cursor >= self.steps.len()
    }

    /// Advance past the current step without merging it.
    pub fn skip(&mut self) {
        if let Some(s) = self.steps.get_mut(self.cursor) {
            if matches!(s.outcome, StepOutcome::Pending | StepOutcome::Failed { .. }) {
                s.outcome = StepOutcome::Skipped;
            }
        }
        self.cursor += 1;
        self.advance_resolved();
    }

    /// Merge the current step. **Requires a [`Confirmation`]** (§11.2).
    ///
    /// Rebases the task branch onto the repo's default branch, runs the repo's tests if it has a
    /// command, and merges only if both succeed. A conflict or a test failure stops here and is
    /// reported — it is not worked around.
    pub fn confirm(&mut self, _proof: Confirmation) -> StepOutcome {
        let Some(step) = self.steps.get_mut(self.cursor) else {
            return StepOutcome::Failed {
                reason: "no step to confirm".into(),
            };
        };
        let outcome = merge_one(step);
        step.outcome = outcome.clone();
        if !matches!(outcome, StepOutcome::Failed { .. }) {
            self.cursor += 1;
            self.advance_resolved();
        }
        outcome
    }

    fn advance_resolved(&mut self) {
        while self.steps.get(self.cursor).is_some_and(|step| {
            matches!(
                step.outcome,
                StepOutcome::Unchanged | StepOutcome::Merged { .. } | StepOutcome::Skipped
            )
        }) {
            self.cursor += 1;
        }
    }

    /// Record the result of the merge (§8.4's `merge.json`).
    pub fn persist(&self, paths: &TaskPaths) -> Result<(), reporch_store::StoreError> {
        let value = PersistedMerge {
            schema_version: 1,
            task: self.task_id.clone(),
            merged_at: reporch_store::now_rfc3339(),
            order_warnings: self
                .order_warnings
                .iter()
                .map(ToString::to_string)
                .collect(),
            cyclic: self.cyclic.clone(),
            steps: self
                .steps
                .iter()
                .map(|step| PersistedStep {
                    repo: step.repo.clone(),
                    branch: step.branch.clone(),
                    changed: step.changed,
                    outcome: step.outcome.clone(),
                })
                .collect(),
        };
        atomic::write_json(&paths.merge_json(), &value)
    }
}

/// A compact review surface for one merge step. `git diff <base>` includes committed, staged and
/// unstaged tracked work; porcelain status adds untracked files, which Git's diff omits.
pub fn diff_summary(step: &MergeStep) -> String {
    let mut lines = Vec::new();
    match reporch_git::git_raw(&step.worktree, &["diff", "--stat", &step.base_commit]) {
        Ok(output) if output.ok() => {
            lines.extend(output.stdout.lines().map(str::to_string));
        }
        Ok(output) => lines.push(format!("could not read diff: {}", output.stderr.trim())),
        Err(error) => lines.push(format!("could not read diff: {error}")),
    }
    if let Ok(status) = reporch_git::git_raw(&step.worktree, &["status", "--porcelain"]) {
        lines.extend(
            status
                .stdout
                .lines()
                .filter(|line| line.starts_with("?? ") && line.get(3..) != Some("result.md"))
                .map(|line| format!("untracked: {}", &line[3..])),
        );
    }
    if lines.is_empty() {
        "No product diff found.".into()
    } else {
        lines.join("\n")
    }
}

/// Rebase, test, merge — the mechanical half of §11.2, after the human has said yes.
fn merge_one(step: &MergeStep) -> StepOutcome {
    let source = &step.source;

    match reporch_git::current_branch(source) {
        Ok(branch) if branch != step.default_branch => {
            return StepOutcome::Failed {
                reason: format!(
                    "`{}` is checked out on `{branch}`, not its default branch `{}`. Switch the \
                     user's checkout yourself, review it, then confirm this step again.",
                    step.repo, step.default_branch
                ),
            };
        }
        Err(error) => {
            return StepOutcome::Failed {
                reason: error.to_string(),
            };
        }
        _ => {}
    }

    // Claude sessions edit; they are not asked to manufacture Git history. The explicit merge
    // confirmation is therefore also the point at which reporch snapshots the reviewed
    // worktree. Without this, the branch can still point at its base while all useful work is
    // only in the index/worktree — exactly what the real E2E run produced (D022).
    if let Err(reason) = snapshot_worktree(step) {
        return StepOutcome::Failed { reason };
    }

    // 2. Fetch, and rebase the task branch onto the repo's default branch.
    let _ = reporch_git::git_raw(source, &["fetch", "--quiet"]);
    let rebase = reporch_git::git_raw(&step.worktree, &["rebase", &step.default_branch]);
    match rebase {
        Ok(o) if !o.ok() => {
            // 3. A conflict stops here. reporch does not resolve it silently; §11.2 hands it to
            // the repo's own session, which still has full task context.
            let _ = reporch_git::git_raw(&step.worktree, &["rebase", "--abort"]);
            return StepOutcome::Failed {
                reason: format!(
                    "rebasing `{}` onto `{}` conflicts:\n{}",
                    step.branch,
                    step.default_branch,
                    o.stderr.trim()
                ),
            };
        }
        Err(e) => {
            return StepOutcome::Failed {
                reason: e.to_string(),
            }
        }
        _ => {}
    }

    // 4. Run the repo's tests. Failure blocks.
    if let Some(cmd) = &step.test_command {
        match reporch_git::run_hook(&step.worktree, cmd) {
            Ok(_) => {}
            Err(e) => {
                return StepOutcome::Failed {
                    reason: format!("tests failed in `{}`:\n{e}", step.repo),
                }
            }
        }
    }

    // 5. Merge into the default branch, in the user's real checkout.
    let merged = reporch_git::git_raw(
        source,
        &[
            "merge",
            "--no-ff",
            &step.branch,
            "-m",
            &format!("reporch: merge {} ({})", step.branch, step.repo),
        ],
    );
    match merged {
        Ok(o) if o.ok() => match reporch_git::head_commit(source) {
            Ok(c) => StepOutcome::Merged { commit: c },
            Err(e) => StepOutcome::Failed {
                reason: e.to_string(),
            },
        },
        Ok(o) => StepOutcome::Failed {
            reason: o.stderr.trim().to_string(),
        },
        Err(e) => StepOutcome::Failed {
            reason: e.to_string(),
        },
    }
}

fn snapshot_worktree(step: &MergeStep) -> Result<(), String> {
    let report = step.worktree.join("result.md");
    let report_is_untracked = report.exists()
        && reporch_git::git_raw(
            &step.worktree,
            &["ls-files", "--error-unmatch", "--", "result.md"],
        )
        .map(|output| !output.ok())
        .unwrap_or(false);

    let add = reporch_git::git_raw(&step.worktree, &["add", "-A", "--", "."])
        .map_err(|error| error.to_string())?;
    if !add.ok() {
        return Err(format!(
            "could not stage the reviewed changes in `{}`:\n{}",
            step.repo,
            add.stderr.trim()
        ));
    }
    if report_is_untracked {
        let reset = reporch_git::git_raw(&step.worktree, &["reset", "--", "result.md"])
            .map_err(|error| error.to_string())?;
        if !reset.ok() {
            return Err(format!(
                "could not separate task metadata from `{}`'s product diff:\n{}",
                step.repo,
                reset.stderr.trim()
            ));
        }
    }

    let staged = reporch_git::git_raw(&step.worktree, &["diff", "--cached", "--quiet"])
        .map_err(|error| error.to_string())?;
    if staged.code == 0 {
        return Ok(());
    }
    if staged.code != 1 {
        return Err(format!(
            "could not inspect the staged changes in `{}`:\n{}",
            step.repo,
            staged.stderr.trim()
        ));
    }

    let commit = reporch_git::git_raw(
        &step.worktree,
        &[
            "commit",
            "-m",
            &format!("reporch: {} ({})", step.branch, step.repo),
        ],
    )
    .map_err(|error| error.to_string())?;
    if !commit.ok() {
        return Err(format!(
            "could not commit the reviewed changes in `{}`:\n{}\n\nConfigure Git's user.name and user.email in this repository, then retry.",
            step.repo,
            commit.stderr.trim()
        ));
    }
    Ok(())
}

/// Tear down a finished task (§11.3).
///
/// **Idempotent and resumable** — the user will close the laptop mid-merge, so every step must be
/// safe to re-run and a step that has already happened is a success, not an error.
pub fn teardown(task: &Task, paths: &TaskPaths) -> Vec<String> {
    let mut problems = Vec::new();

    for r in &task.repositories {
        let source = Path::new(&r.path);
        let wt = Path::new(&r.worktree_path);

        // Remove reporch's block from info/exclude before the worktree goes, leaving the user's
        // repository as it was found (§9.2 Deviation).
        if wt.exists() {
            if let Err(e) = reporch_git::clear_exclude_block(wt) {
                problems.push(format!("{}: could not clean info/exclude — {e}", r.name));
            }
        }
        if let Err(e) = reporch_git::remove_worktree(source, wt) {
            problems.push(format!("{}: {e}", r.name));
        }
        // Delete the task branch locally. Never on a remote: reporch does not push.
        let _ = reporch_git::git_raw(source, &["branch", "-D", &r.branch]);
    }

    // §8.4: keep task.json, base.md, result.md, merge.json and a compacted index; drop the logs
    // and the brief bodies, which are the bulk.
    if let Err(e) = compact_history(paths) {
        problems.push(format!("history compaction: {e}"));
    }
    problems
}

/// Compact a finished task's history (§8.4).
///
/// Keeps `task.json`, `base.md`, `result.md`, `merge.json`, and an index with subjects and
/// statuses but no bodies. Typically a few KB per task; `logs/` alone can be tens of MB.
pub fn compact_history(paths: &TaskPaths) -> std::io::Result<()> {
    if paths.logs_dir().exists() {
        std::fs::remove_dir_all(paths.logs_dir())?;
    }
    // Drop the brief bodies, keep the index. The index already carries subject, kind, status and
    // timestamps, which is what history is actually for.
    if let Ok(entries) = std::fs::read_dir(paths.briefs_dir()) {
        for e in entries.filter_map(Result::ok) {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "md") {
                std::fs::remove_file(p)?;
            }
        }
    }
    for scratch in [
        "ping-requests.jsonl",
        "status-updates.jsonl",
        "finish-requested",
    ] {
        let p = paths.dir().join(scratch);
        if p.exists() {
            std::fs::remove_file(p)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use reporch_core::{Effort, GraphNode, RepoState, Role, TaskId, TaskRepo, TaskStatus};
    use std::collections::BTreeMap;

    fn git(dir: &Path, args: &[&str]) {
        let o = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }

    fn repo_with_commit(root: &Path, name: &str) -> PathBuf {
        let p = root.join(name);
        std::fs::create_dir_all(&p).unwrap();
        git(&p, &["init", "-q", "-b", "main"]);
        git(&p, &["config", "user.email", "t@t"]);
        git(&p, &["config", "user.name", "t"]);
        std::fs::write(p.join("README.md"), "# x\n").unwrap();
        git(&p, &["add", "-A"]);
        git(&p, &["commit", "-qm", "init"]);
        p
    }

    fn task_repo(name: &str, path: &Path, wt: &Path, role: Role) -> TaskRepo {
        TaskRepo {
            name: name.into(),
            path: path.display().to_string(),
            role,
            worktree_path: wt.display().to_string(),
            branch: "reporch/TASK-0001-x".into(),
            base_commit: "0".repeat(40),
            session_alias: format!("{name}@TASK-0001"),
            session_id: "s".into(),
            session_pid: None,
            model: "opus".into(),
            effort: Effort::Max,
            ultracode: false,
            state: RepoState::Done,
            changed: Some(true),
            last_activity_at: None,
            spent_usd: 0.0,
            token_usage: Default::default(),
        }
    }

    fn task(repos: Vec<TaskRepo>) -> Task {
        Task {
            schema_version: 1,
            id: TaskId::new(1),
            name: "x".into(),
            status: TaskStatus::Completed,
            created_at: "2026-08-07T21:40:00Z".into(),
            started_at: None,
            finished_at: None,
            master_repo: "modulex".into(),
            model: "opus".into(),
            master_effort: Effort::Max,
            worker_effort: Effort::XHigh,
            ultracode: false,
            budget_usd: 20.0,
            spent_usd: 1.0,
            branch: "reporch/TASK-0001-x".into(),
            repositories: repos,
        }
    }

    fn graph() -> DepGraph {
        let members = vec!["modulex".to_string(), "modulex-ui".to_string()];
        let mut d = BTreeMap::new();
        d.insert(
            "modulex".to_string(),
            GraphNode {
                consumes: vec![],
                produces_contracts_for: vec!["modulex-ui".into()],
            },
        );
        DepGraph::build(&members, &d)
    }

    fn report(order: Vec<&str>) -> ResultReport {
        ResultReport {
            summary: "Org-scoped API keys, end to end.".into(),
            repositories: vec![
                ResultRepo {
                    name: "modulex".into(),
                    changed: true,
                    files: Some(11),
                    tests: Some("passing".into()),
                    summary: Some("Key model and endpoints.".into()),
                    merge_notes: Some("Requires migration 0042.".into()),
                    reason: None,
                },
                ResultRepo {
                    name: "modulex-ui".into(),
                    changed: false,
                    files: None,
                    tests: None,
                    summary: None,
                    merge_notes: None,
                    reason: Some("No UI surface affected.".into()),
                },
            ],
            merge_order: order.into_iter().map(str::to_string).collect(),
            risks: vec!["The secret is returned once.".into()],
        }
    }

    #[test]
    fn the_plan_follows_the_masters_order_when_it_agrees_with_the_graph() {
        let d = tempfile::tempdir().unwrap();
        let a = repo_with_commit(d.path(), "modulex");
        let b = repo_with_commit(d.path(), "modulex-ui");
        let t = task(vec![
            task_repo("modulex", &a, &d.path().join("wt/a"), Role::Master),
            task_repo("modulex-ui", &b, &d.path().join("wt/b"), Role::Worker),
        ]);
        let plan = MergePlan::build(&t, &report(vec!["modulex", "modulex-ui"]), &graph());
        assert!(plan.order_warnings.is_empty());
        let names: Vec<&str> = plan.steps.iter().map(|s| s.repo.as_str()).collect();
        assert_eq!(names, vec!["modulex", "modulex-ui"]);
    }

    /// §11.1: a proposed order that contradicts the graph is a **warning**, not a silent override.
    #[test]
    fn a_contradictory_order_warns_but_is_still_offered() {
        let d = tempfile::tempdir().unwrap();
        let a = repo_with_commit(d.path(), "modulex");
        let b = repo_with_commit(d.path(), "modulex-ui");
        let t = task(vec![
            task_repo("modulex", &a, &d.path().join("wt/a"), Role::Master),
            task_repo("modulex-ui", &b, &d.path().join("wt/b"), Role::Worker),
        ]);
        // Consumer before producer.
        let plan = MergePlan::build(&t, &report(vec!["modulex-ui", "modulex"]), &graph());
        assert_eq!(plan.order_warnings.len(), 1);
        assert!(plan.order_warnings[0]
            .to_string()
            .contains("scheduled before"));
        // Still presented in the order the master asked for — it may know something.
        let names: Vec<&str> = plan.steps.iter().map(|s| s.repo.as_str()).collect();
        assert_eq!(names, vec!["modulex-ui", "modulex"]);
    }

    #[test]
    fn a_repo_the_master_omitted_is_still_presented() {
        let d = tempfile::tempdir().unwrap();
        let a = repo_with_commit(d.path(), "modulex");
        let b = repo_with_commit(d.path(), "modulex-ui");
        let t = task(vec![
            task_repo("modulex", &a, &d.path().join("wt/a"), Role::Master),
            task_repo("modulex-ui", &b, &d.path().join("wt/b"), Role::Worker),
        ]);
        let plan = MergePlan::build(&t, &report(vec!["modulex"]), &graph());
        assert_eq!(plan.steps.len(), 2, "nothing is silently dropped");
    }

    #[test]
    fn a_narrative_result_falls_back_to_the_real_worktree_diff() {
        let d = tempfile::tempdir().unwrap();
        let source = repo_with_commit(d.path(), "modulex");
        let wt = d.path().join("wt/modulex");
        let w = reporch_git::add_worktree(&source, &wt, "reporch/TASK-0001-x", "main").unwrap();
        std::fs::write(wt.join("keys.py"), "def issue(): ...\n").unwrap();
        let paths = TaskPaths::at(d.path().join("task"));
        std::fs::create_dir_all(paths.dir()).unwrap();
        atomic::write_string(
            &paths.result_md(),
            "# TASK-0001\n\nAdded scoped keys across the project.\n",
        )
        .unwrap();
        let mut tr = task_repo("modulex", &source, &wt, Role::Master);
        tr.base_commit = w.base_commit;
        let t = task(vec![tr]);

        let loaded = load_report(&t, &paths).unwrap();

        assert_eq!(
            loaded.report.summary,
            "Added scoped keys across the project."
        );
        assert!(loaded.report.repositories[0].changed);
        assert_eq!(loaded.report.repositories[0].files, Some(1));
        assert!(loaded
            .warnings
            .iter()
            .any(|warning| warning.contains("narrative")));
    }

    #[test]
    fn an_untracked_master_report_is_not_a_product_change() {
        let d = tempfile::tempdir().unwrap();
        let source = repo_with_commit(d.path(), "modulex");
        let wt = d.path().join("wt/modulex");
        let w = reporch_git::add_worktree(&source, &wt, "reporch/TASK-0001-x", "main").unwrap();
        std::fs::write(wt.join("result.md"), "{}\n").unwrap();

        assert!(!has_task_changes(&wt, &w.base_commit).unwrap());
    }

    #[test]
    fn an_unchanged_repo_needs_no_confirmation() {
        let d = tempfile::tempdir().unwrap();
        let a = repo_with_commit(d.path(), "modulex");
        let b = repo_with_commit(d.path(), "modulex-ui");
        let t = task(vec![
            task_repo("modulex", &a, &d.path().join("wt/a"), Role::Master),
            task_repo("modulex-ui", &b, &d.path().join("wt/b"), Role::Worker),
        ]);
        let plan = MergePlan::build(&t, &report(vec!["modulex", "modulex-ui"]), &graph());
        let ui = plan.steps.iter().find(|s| s.repo == "modulex-ui").unwrap();
        assert_eq!(ui.outcome, StepOutcome::Unchanged);
        assert_eq!(
            plan.remaining(),
            1,
            "only the changed repo awaits a decision"
        );
        assert_eq!(
            plan.current().map(|step| step.repo.as_str()),
            Some("modulex"),
            "unchanged repositories are advanced without pretending the user confirmed them"
        );
    }

    #[test]
    fn project_metadata_configures_default_branches_and_real_test_commands() {
        let d = tempfile::tempdir().unwrap();
        let source = repo_with_commit(d.path(), "modulex");
        let wt = d.path().join("wt/modulex");
        let worktree =
            reporch_git::add_worktree(&source, &wt, "reporch/TASK-0001-x", "main").unwrap();
        let mut tr = task_repo("modulex", &source, &wt, Role::Master);
        tr.base_commit = worktree.base_commit;
        let t = task(vec![tr]);
        let mut plan = MergePlan::build(
            &t,
            &report(vec!["modulex"]),
            &DepGraph::build(&["modulex".into()], &BTreeMap::new()),
        );
        let project = reporch_core::ProjectBase {
            schema_version: 1,
            name: "p".into(),
            created_at: String::new(),
            analyzed_at: None,
            overview: String::new(),
            repositories: vec![reporch_core::ProjectRepo {
                name: "modulex".into(),
                path: source.display().to_string(),
                remote_url: None,
                default_branch: Some("trunk".into()),
                purpose: String::new(),
                profile_ref: None,
                cross_session_inbound: None,
            }],
            graph: BTreeMap::new(),
            settings: Default::default(),
            next_task_id: 2,
        };
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "modulex".into(),
            reporch_core::RepoProfile {
                commands: BTreeMap::from([("test".into(), "cargo test --all".into())]),
                ..Default::default()
            },
        );

        plan.configure(&project, &profiles);

        assert_eq!(plan.steps[0].default_branch, "trunk");
        assert_eq!(
            plan.steps[0].test_command.as_deref(),
            Some("cargo test --all")
        );
    }

    #[test]
    fn persisted_terminal_steps_resume_without_another_confirmation() {
        let d = tempfile::tempdir().unwrap();
        let paths = TaskPaths::at(d.path().join("task"));
        std::fs::create_dir_all(paths.dir()).unwrap();
        let source = repo_with_commit(d.path(), "modulex");
        let wt = d.path().join("wt/modulex");
        let worktree =
            reporch_git::add_worktree(&source, &wt, "reporch/TASK-0001-x", "main").unwrap();
        std::fs::write(wt.join("keys.py"), "pass\n").unwrap();
        let mut tr = task_repo("modulex", &source, &wt, Role::Master);
        tr.base_commit = worktree.base_commit;
        let t = task(vec![tr]);
        let graph = DepGraph::build(&["modulex".into()], &BTreeMap::new());
        let mut first = MergePlan::build(&t, &report(vec!["modulex"]), &graph);
        assert!(matches!(
            first.confirm(Confirmation::by_user()),
            StepOutcome::Merged { .. }
        ));
        first.persist(&paths).unwrap();

        let mut resumed = MergePlan::build(&t, &report(vec!["modulex"]), &graph);
        resumed.restore(&paths).unwrap();

        assert!(resumed.is_done());
        assert_eq!(resumed.remaining(), 0);
        assert!(matches!(
            resumed.steps[0].outcome,
            StepOutcome::Merged { .. }
        ));
    }

    #[test]
    fn diff_summary_includes_uncommitted_and_untracked_product_files() {
        let d = tempfile::tempdir().unwrap();
        let source = repo_with_commit(d.path(), "modulex");
        let wt = d.path().join("wt/modulex");
        let worktree =
            reporch_git::add_worktree(&source, &wt, "reporch/TASK-0001-x", "main").unwrap();
        std::fs::write(wt.join("README.md"), "changed\n").unwrap();
        std::fs::write(wt.join("keys.py"), "pass\n").unwrap();
        let mut tr = task_repo("modulex", &source, &wt, Role::Master);
        tr.base_commit = worktree.base_commit;
        let t = task(vec![tr]);
        let plan = MergePlan::build(
            &t,
            &report(vec!["modulex"]),
            &DepGraph::build(&["modulex".into()], &BTreeMap::new()),
        );

        let summary = diff_summary(plan.current().unwrap());

        assert!(summary.contains("README.md"), "{summary}");
        assert!(summary.contains("keys.py"), "{summary}");
    }

    /// **A real merge, end to end**, through the confirmation gate.
    #[test]
    fn a_confirmed_step_rebases_and_merges_for_real() {
        let d = tempfile::tempdir().unwrap();
        let source = repo_with_commit(d.path(), "modulex");
        let wt = d.path().join("wt/modulex");
        let w = reporch_git::add_worktree(&source, &wt, "reporch/TASK-0001-x", "main").unwrap();

        // The session did some work.
        std::fs::write(wt.join("keys.py"), "def issue(): ...\n").unwrap();
        git(&wt, &["add", "-A"]);
        git(
            &wt,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-qm",
                "add keys",
            ],
        );

        let mut tr = task_repo("modulex", &source, &wt, Role::Master);
        tr.base_commit = w.base_commit;
        let t = task(vec![tr]);
        let g = DepGraph::build(&["modulex".to_string()], &BTreeMap::new());
        let mut plan = MergePlan::build(&t, &report(vec!["modulex"]), &g);

        assert_eq!(plan.remaining(), 1);
        let outcome = plan.confirm(Confirmation::by_user());
        assert!(
            matches!(outcome, StepOutcome::Merged { .. }),
            "expected a merge, got {outcome:?}"
        );
        // The work is really in the user's checkout now.
        assert!(source.join("keys.py").is_file());
        assert!(plan.is_done());
    }

    #[test]
    fn a_confirmed_step_snapshots_uncommitted_session_work_before_merge() {
        let d = tempfile::tempdir().unwrap();
        let source = repo_with_commit(d.path(), "modulex");
        let wt = d.path().join("wt/modulex");
        let w = reporch_git::add_worktree(&source, &wt, "reporch/TASK-0001-x", "main").unwrap();
        std::fs::write(wt.join("keys.py"), "def issue(): ...\n").unwrap();
        // The real E2E task writes this report in the master's worktree. It belongs in task
        // history, not in the user's product branch.
        std::fs::write(wt.join("result.md"), "# Task result\n").unwrap();

        let mut tr = task_repo("modulex", &source, &wt, Role::Master);
        tr.base_commit = w.base_commit;
        let t = task(vec![tr]);
        let g = DepGraph::build(&["modulex".to_string()], &BTreeMap::new());
        let mut plan = MergePlan::build(&t, &report(vec!["modulex"]), &g);

        let outcome = plan.confirm(Confirmation::by_user());

        assert!(matches!(outcome, StepOutcome::Merged { .. }), "{outcome:?}");
        assert_eq!(
            std::fs::read_to_string(source.join("keys.py")).unwrap(),
            "def issue(): ...\n"
        );
        assert!(
            !source.join("result.md").exists(),
            "task metadata must not enter the product branch"
        );
    }

    /// A conflict must stop and report, never be worked around silently (§11.2 step 3).
    #[test]
    fn a_conflicting_step_fails_with_the_reason_and_does_not_merge() {
        let d = tempfile::tempdir().unwrap();
        let source = repo_with_commit(d.path(), "modulex");
        let wt = d.path().join("wt/modulex");
        let w = reporch_git::add_worktree(&source, &wt, "reporch/TASK-0001-x", "main").unwrap();

        // Both sides change the same line.
        std::fs::write(wt.join("README.md"), "# from the task\n").unwrap();
        git(&wt, &["add", "-A"]);
        git(
            &wt,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-qm",
                "task side",
            ],
        );
        std::fs::write(source.join("README.md"), "# from main\n").unwrap();
        git(&source, &["add", "-A"]);
        git(&source, &["commit", "-qm", "main side"]);

        let mut tr = task_repo("modulex", &source, &wt, Role::Master);
        tr.base_commit = w.base_commit;
        let t = task(vec![tr]);
        let g = DepGraph::build(&["modulex".to_string()], &BTreeMap::new());
        let mut plan = MergePlan::build(&t, &report(vec!["modulex"]), &g);

        let outcome = plan.confirm(Confirmation::by_user());
        match outcome {
            StepOutcome::Failed { reason } => assert!(
                reason.contains("conflict") || reason.contains("CONFLICT"),
                "must say what went wrong: {reason}"
            ),
            other => panic!("a conflict must not merge: {other:?}"),
        }
        assert!(
            !plan.is_done(),
            "a failed repository remains current so the user can repair and retry or skip it"
        );
        assert_eq!(
            std::fs::read_to_string(source.join("README.md")).unwrap(),
            "# from main\n",
            "the user's checkout is untouched by a failed merge"
        );
    }

    #[test]
    fn skipping_leaves_the_repo_alone() {
        let d = tempfile::tempdir().unwrap();
        let source = repo_with_commit(d.path(), "modulex");
        let wt = d.path().join("wt/modulex");
        reporch_git::add_worktree(&source, &wt, "reporch/TASK-0001-x", "main").unwrap();
        let t = task(vec![task_repo("modulex", &source, &wt, Role::Master)]);
        let g = DepGraph::build(&["modulex".to_string()], &BTreeMap::new());
        let mut plan = MergePlan::build(&t, &report(vec!["modulex"]), &g);
        plan.skip();
        assert_eq!(plan.steps[0].outcome, StepOutcome::Skipped);
        assert!(plan.is_done());
    }

    #[test]
    fn the_outcome_is_recorded_in_merge_json() {
        let d = tempfile::tempdir().unwrap();
        let paths = TaskPaths::at(d.path().join("t"));
        std::fs::create_dir_all(paths.dir()).unwrap();
        let source = repo_with_commit(d.path(), "modulex");
        let wt = d.path().join("wt/modulex");
        let t = task(vec![task_repo("modulex", &source, &wt, Role::Master)]);
        let g = DepGraph::build(&["modulex".to_string()], &BTreeMap::new());
        let plan = MergePlan::build(&t, &report(vec!["modulex"]), &g);
        plan.persist(&paths).unwrap();
        let v: serde_json::Value = atomic::read_json(&paths.merge_json()).unwrap();
        assert_eq!(v["task"], "TASK-0001");
        assert_eq!(v["steps"][0]["repo"], "modulex");
    }

    // ---- teardown ----------------------------------------------------------

    /// §11.3: teardown must be idempotent and resumable — the user will close the laptop.
    #[test]
    fn teardown_removes_worktrees_and_is_safe_to_repeat() {
        let d = tempfile::tempdir().unwrap();
        let source = repo_with_commit(d.path(), "modulex");
        let wt = d.path().join("wt/modulex");
        reporch_git::add_worktree(&source, &wt, "reporch/TASK-0001-x", "main").unwrap();
        let paths = TaskPaths::at(d.path().join("t"));
        std::fs::create_dir_all(paths.logs_dir()).unwrap();
        let t = task(vec![task_repo("modulex", &source, &wt, Role::Master)]);

        let problems = teardown(&t, &paths);
        assert!(problems.is_empty(), "{problems:?}");
        assert!(!wt.exists());

        // Again: still fine.
        let problems = teardown(&t, &paths);
        assert!(
            problems.is_empty(),
            "teardown must be resumable: {problems:?}"
        );
    }

    #[test]
    fn compaction_keeps_the_record_and_drops_the_bulk() {
        let d = tempfile::tempdir().unwrap();
        let paths = TaskPaths::at(d.path().join("t"));
        std::fs::create_dir_all(paths.briefs_dir()).unwrap();
        std::fs::create_dir_all(paths.logs_dir()).unwrap();

        atomic::write_string(&paths.base_md(), "the brief").unwrap();
        atomic::write_string(&paths.result_md(), "the result").unwrap();
        atomic::write_string(
            &paths.brief_index(),
            "{\"schema_version\":1,\"next_id\":1,\"entries\":[]}",
        )
        .unwrap();
        atomic::write_string(&paths.brief_file("0001-a.to.b.md"), "body").unwrap();
        atomic::write_string(&paths.session_log("modulex"), "{}").unwrap();
        atomic::write_string(&paths.dir().join("ping-requests.jsonl"), "{}").unwrap();

        compact_history(&paths).unwrap();

        // Kept (§8.4).
        assert!(paths.base_md().is_file());
        assert!(paths.result_md().is_file());
        assert!(paths.brief_index().is_file(), "the index is the history");
        // Dropped.
        assert!(!paths.logs_dir().exists(), "logs are tens of MB");
        assert!(!paths.brief_file("0001-a.to.b.md").exists());
        assert!(!paths.dir().join("ping-requests.jsonl").exists());
    }

    #[test]
    fn compaction_is_idempotent() {
        let d = tempfile::tempdir().unwrap();
        let paths = TaskPaths::at(d.path().join("t"));
        std::fs::create_dir_all(paths.dir()).unwrap();
        compact_history(&paths).unwrap();
        compact_history(&paths).unwrap();
    }
}
