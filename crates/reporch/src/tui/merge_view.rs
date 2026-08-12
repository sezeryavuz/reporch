//! Completed-task review and merge execution (§11).
//!
//! Rendering is pure. Every Git mutation runs on the console's blocking-work pool and arrives
//! here as a new immutable-looking state value.

use std::collections::BTreeMap;

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Padding, Paragraph, Wrap};
use ratatui::Frame;

use reporch_core::{DepGraph, ProjectBase, RepoProfile, Task, TaskStatus};
use reporch_store::{atomic, Paths, TaskPaths};

use crate::project::catalog::TaskRecord;
use crate::task::{self, Confirmation, LoadedReport, MergePlan, StepOutcome};

use super::theme;

#[derive(Debug)]
pub struct MergeState {
    pub task: Task,
    pub paths: TaskPaths,
    pub report: Option<LoadedReport>,
    pub plan: Option<MergePlan>,
    pub current_diff: String,
    pub loading: bool,
    pub busy: bool,
    pub confirm_skip: bool,
    pub message: Option<String>,
    pub recovery_error: Option<String>,
    pub cleanup_problems: Vec<String>,
    pub complete: bool,
    pub detail_scroll: u16,
}

impl MergeState {
    pub fn loading(record: &TaskRecord) -> Self {
        Self {
            task: record.task.clone(),
            paths: record.paths.clone(),
            report: None,
            plan: None,
            current_diff: String::new(),
            loading: true,
            busy: false,
            confirm_skip: false,
            message: None,
            recovery_error: None,
            cleanup_problems: Vec::new(),
            complete: false,
            detail_scroll: 0,
        }
    }
}

#[derive(Debug)]
pub struct PreparedMerge {
    pub report: LoadedReport,
    pub plan: MergePlan,
    pub current_diff: String,
}

#[derive(Debug)]
pub enum MergeAction {
    Confirm(Confirmation),
    Skip,
    /// Retry an interrupted `merge.json` write or idempotent teardown without replaying a merge.
    Finalize,
}

#[derive(Debug)]
pub struct MergeUpdate {
    pub task: Task,
    pub plan: MergePlan,
    pub current_diff: String,
    pub message: String,
    pub recovery_error: Option<String>,
    pub cleanup_problems: Vec<String>,
    pub complete: bool,
}

/// Load the report, graph, per-repository test commands and any interrupted merge record.
pub fn prepare(
    paths: &Paths,
    project: &ProjectBase,
    task: &Task,
    task_paths: &TaskPaths,
) -> Result<PreparedMerge, String> {
    let mut report = task::load_report(task, task_paths).map_err(|error| error.to_string())?;
    let graph = DepGraph::build(&project.repo_names(), &project.graph);
    let mut profiles = BTreeMap::new();
    for repo in &project.repositories {
        match atomic::read_json_opt::<RepoProfile>(
            &paths.repo_profile_json(&project.name, &repo.name),
        ) {
            Ok(Some(profile)) => {
                profiles.insert(repo.name.clone(), profile);
            }
            Ok(None) => report.warnings.push(format!(
                "{} has no analysis profile; no repository test command can be run",
                repo.name
            )),
            Err(error) => report.warnings.push(format!(
                "{}'s analysis profile is unreadable; no repository test command can be run: {error}",
                repo.name
            )),
        }
    }
    let mut plan = MergePlan::build(task, &report.report, &graph);
    plan.configure(project, &profiles);
    plan.restore(task_paths)
        .map_err(|error| error.to_string())?;
    let current_diff = plan.current().map(task::diff_summary).unwrap_or_default();
    Ok(PreparedMerge {
        report,
        plan,
        current_diff,
    })
}

/// Execute one decision. The `Confirmation` in `MergeAction::Confirm` can only come from the
/// console's literal `y` key handler. This function has no implicit-confirm path.
pub fn execute(
    mut plan: MergePlan,
    task: Task,
    paths: &TaskPaths,
    action: MergeAction,
) -> MergeUpdate {
    let message = match action {
        MergeAction::Confirm(proof) => match plan.confirm(proof) {
            StepOutcome::Merged { commit } => {
                format!("merged at {}", &commit[..commit.len().min(12)])
            }
            StepOutcome::Failed { reason } => format!("merge blocked: {reason}"),
            other => format!("merge returned {other:?}"),
        },
        MergeAction::Skip => {
            let repo = plan
                .current()
                .map(|step| step.repo.clone())
                .unwrap_or_else(|| "repository".into());
            plan.skip();
            format!("skipped `{repo}` by explicit user choice")
        }
        MergeAction::Finalize => "retried merge persistence and cleanup".into(),
    };

    if let Err(error) = plan.persist(paths) {
        return update(
            plan,
            task,
            message,
            Some(format!("could not persist merge.json: {error}")),
            Vec::new(),
            false,
        );
    }

    if !plan.is_done() {
        return update(plan, task, message, None, Vec::new(), false);
    }

    let mut problems = task::merge::teardown(&task, paths);
    if problems.is_empty() {
        let mut merged = task.clone();
        merged.status = TaskStatus::Merged;
        if let Err(error) = atomic::write_json(&paths.task_json(), &merged) {
            problems.push(format!("could not mark task merged: {error}"));
        } else {
            return update(plan, merged, message, None, Vec::new(), true);
        }
    }
    update(
        plan,
        task,
        message,
        Some("merge steps are recorded, but teardown is incomplete; press r to retry".into()),
        problems.clone(),
        false,
    )
}

fn update(
    plan: MergePlan,
    task: Task,
    message: String,
    recovery_error: Option<String>,
    cleanup_problems: Vec<String>,
    complete: bool,
) -> MergeUpdate {
    let current_diff = plan.current().map(task::diff_summary).unwrap_or_default();
    MergeUpdate {
        task,
        plan,
        current_diff,
        message,
        recovery_error,
        cleanup_problems,
        complete,
    }
}

pub fn draw(frame: &mut Frame, area: Rect, state: &MergeState) {
    let [header, steps, detail, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(6),
        Constraint::Min(5),
        Constraint::Length(2),
    ])
    .areas(area);

    let summary = state
        .report
        .as_ref()
        .map(|loaded| loaded.report.summary.as_str())
        .unwrap_or("Loading the completion report and Git state…");
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::raw("  "),
                Span::raw(format!("{} · merge review", state.task.id))
                    .bold()
                    .fg(theme::ACCENT),
            ]),
            Line::from(Span::raw(format!("  {summary}")).fg(theme::DIM)),
        ])
        .wrap(Wrap { trim: false }),
        header,
    );

    let step_lines = state
        .plan
        .as_ref()
        .map(|plan| {
            plan.steps
                .iter()
                .map(|step| {
                    let marker = if plan
                        .current()
                        .is_some_and(|current| current.repo == step.repo)
                    {
                        "›"
                    } else {
                        " "
                    };
                    Line::from(vec![
                        Span::raw(format!("  {marker} {:<22}", step.repo)),
                        Span::raw(outcome_label(&step.outcome)).fg(outcome_colour(&step.outcome)),
                    ])
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| vec![Line::from("  reading task artifacts…")]);
    frame.render_widget(Paragraph::new(step_lines), steps);

    let mut lines = Vec::new();
    if let Some(report) = &state.report {
        for risk in &report.report.risks {
            lines.push(Line::from(
                Span::raw(format!("reported risk: {risk}")).fg(theme::WARN),
            ));
        }
    }
    if let Some(plan) = &state.plan {
        for warning in &plan.order_warnings {
            lines.push(Line::from(
                Span::raw(format!("order warning: {warning}")).fg(theme::WARN),
            ));
        }
        if !plan.cyclic.is_empty() {
            lines.push(Line::from(
                Span::raw(format!("dependency cycle: {}", plan.cyclic.join(", "))).fg(theme::WARN),
            ));
        }
        if let Some(step) = plan.current() {
            lines.push(Line::from(Span::raw(format!(
                "repository: {}  ·  branch: {} → {}",
                step.repo, step.branch, step.default_branch
            ))));
            lines.push(Line::from(Span::raw(format!(
                "tests: {}",
                step.test_command.as_deref().unwrap_or("none recorded")
            ))));
            lines.push(Line::from(Span::raw(format!(
                "merge notes: {}",
                step.merge_notes.as_deref().unwrap_or("none")
            ))));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::raw("diff summary").bold()));
            lines.extend(
                state
                    .current_diff
                    .lines()
                    .map(|line| Line::from(line.to_string())),
            );
            if let StepOutcome::Failed { reason } = &step.outcome {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::raw(reason.clone()).fg(theme::FAIL)));
            }
        }
    }
    if let Some(report) = &state.report {
        for warning in &report.warnings {
            lines.push(Line::from(
                Span::raw(format!("warning: {warning}")).fg(theme::WARN),
            ));
        }
    }
    if let Some(error) = &state.recovery_error {
        lines.push(Line::from(Span::raw(error.clone()).fg(theme::FAIL)));
    }
    if let Some(message) = &state.message {
        lines.push(Line::from(Span::raw(message.clone()).fg(theme::ACCENT)));
    }
    lines.extend(
        state
            .cleanup_problems
            .iter()
            .map(|problem| Line::from(Span::raw(problem.clone()).fg(theme::FAIL))),
    );
    if state.complete {
        lines.push(Line::from(
            Span::raw("All decisions persisted; worktrees removed and history compacted.")
                .fg(theme::OK),
        ));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().padding(Padding::new(2, 2, 0, 0)))
            .scroll((state.detail_scroll, 0))
            .wrap(Wrap { trim: false }),
        detail,
    );

    let hint: String = if state.loading {
        "  loading merge plan…".into()
    } else if state.busy {
        "  applying the confirmed decision…".into()
    } else if state.confirm_skip {
        "  Skipping discards this worktree during teardown. y confirm · n/esc cancel".into()
    } else if state.recovery_error.is_some() {
        "  r retry persistence/cleanup · esc completed tasks".into()
    } else if state.complete {
        "  esc completed tasks".into()
    } else if state.plan.as_ref().is_some_and(MergePlan::is_done) {
        "  r finish idempotent teardown · esc completed tasks".into()
    } else {
        "  y confirm merge/retry · s skip · j/k scroll · esc completed tasks".into()
    };
    frame.render_widget(
        Paragraph::new(hint).style(if state.confirm_skip {
            ratatui::style::Style::default().fg(theme::WARN)
        } else {
            ratatui::style::Style::default().fg(theme::DIM)
        }),
        footer,
    );
}

fn outcome_label(outcome: &StepOutcome) -> &'static str {
    match outcome {
        StepOutcome::Pending => "pending confirmation",
        StepOutcome::Unchanged => "unchanged",
        StepOutcome::Merged { .. } => "merged",
        StepOutcome::Skipped => "skipped",
        StepOutcome::Failed { .. } => "blocked · retry or skip",
    }
}

fn outcome_colour(outcome: &StepOutcome) -> ratatui::style::Color {
    match outcome {
        StepOutcome::Pending => theme::ACCENT,
        StepOutcome::Unchanged => theme::DIM,
        StepOutcome::Merged { .. } => theme::OK,
        StepOutcome::Skipped => theme::WARN,
        StepOutcome::Failed { .. } => theme::FAIL,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reporch_core::{Effort, RepoState, Role, TaskId, TaskRepo};
    use std::path::{Path, PathBuf};

    fn git(dir: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn fixture(root: &Path) -> (PathBuf, PathBuf, Task) {
        let source = root.join("repo");
        std::fs::create_dir_all(&source).unwrap();
        git(&source, &["init", "-q", "-b", "main"]);
        git(&source, &["config", "user.name", "fixture"]);
        git(&source, &["config", "user.email", "fixture@example.test"]);
        std::fs::write(source.join("README.md"), "before\n").unwrap();
        git(&source, &["add", "-A"]);
        git(&source, &["commit", "-qm", "initial"]);
        let worktree = root.join("worktree");
        let created =
            reporch_git::add_worktree(&source, &worktree, "reporch/TASK-0001-x", "main").unwrap();
        std::fs::write(worktree.join("feature.txt"), "real session output\n").unwrap();
        let task = Task {
            schema_version: 1,
            id: TaskId::new(1),
            name: "x".into(),
            status: TaskStatus::Completed,
            created_at: reporch_store::now_rfc3339(),
            started_at: None,
            finished_at: Some(reporch_store::now_rfc3339()),
            master_repo: "repo".into(),
            model: "sonnet".into(),
            master_effort: Effort::Medium,
            worker_effort: Effort::Low,
            ultracode: false,
            budget_usd: 1.0,
            spent_usd: 0.1,
            branch: "reporch/TASK-0001-x".into(),
            repositories: vec![TaskRepo {
                name: "repo".into(),
                path: source.display().to_string(),
                role: Role::Master,
                worktree_path: worktree.display().to_string(),
                branch: "reporch/TASK-0001-x".into(),
                base_commit: created.base_commit,
                session_alias: "repo@TASK-0001".into(),
                session_id: "session".into(),
                session_pid: None,
                model: "sonnet".into(),
                effort: Effort::Medium,
                ultracode: false,
                state: RepoState::Done,
                changed: Some(true),
                last_activity_at: None,
                spent_usd: 0.1,
                token_usage: Default::default(),
            }],
        };
        (source, worktree, task)
    }

    #[test]
    fn a_console_merge_decision_merges_real_output_then_tears_down_and_marks_history() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::at(temp.path().join("home"));
        let task_paths = TaskPaths::at(paths.task_dir("project", "x", TaskId::new(1)));
        std::fs::create_dir_all(task_paths.dir()).unwrap();
        let (source, worktree, task) = fixture(temp.path());
        atomic::write_json(&task_paths.task_json(), &task).unwrap();
        atomic::write_string(
            &task_paths.result_md(),
            "Completed the real fixture change across the project.\n",
        )
        .unwrap();
        let project = ProjectBase {
            schema_version: 1,
            name: "project".into(),
            created_at: reporch_store::now_rfc3339(),
            analyzed_at: None,
            overview: String::new(),
            repositories: vec![reporch_core::ProjectRepo {
                name: "repo".into(),
                path: source.display().to_string(),
                remote_url: None,
                default_branch: Some("main".into()),
                purpose: String::new(),
                profile_ref: None,
                cross_session_inbound: None,
            }],
            graph: BTreeMap::new(),
            settings: Default::default(),
            next_task_id: 2,
        };
        let prepared = prepare(&paths, &project, &task, &task_paths).unwrap();
        let review = MergeState {
            task: task.clone(),
            paths: task_paths.clone(),
            report: Some(prepared.report.clone()),
            plan: Some(prepared.plan.clone()),
            current_diff: prepared.current_diff.clone(),
            loading: false,
            busy: false,
            confirm_skip: false,
            message: None,
            recovery_error: None,
            cleanup_problems: Vec::new(),
            complete: false,
            detail_scroll: 0,
        };
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| draw(frame, frame.area(), &review))
            .unwrap();
        let rendered = terminal.backend().buffer().content().iter().fold(
            String::new(),
            |mut rendered, cell| {
                rendered.push_str(cell.symbol());
                rendered
            },
        );
        assert!(
            rendered.contains("feature.txt"),
            "a 24-row review must show the diff before compatibility warnings: {rendered}"
        );

        let update = execute(
            prepared.plan,
            task,
            &task_paths,
            MergeAction::Confirm(Confirmation::by_user()),
        );

        assert!(update.complete, "{:?}", update.cleanup_problems);
        assert_eq!(update.task.status, TaskStatus::Merged);
        assert_eq!(
            std::fs::read_to_string(source.join("feature.txt")).unwrap(),
            "real session output\n"
        );
        assert!(!worktree.exists());
        assert!(task_paths.merge_json().is_file());
        let persisted: Task = atomic::read_json(&task_paths.task_json()).unwrap();
        assert_eq!(persisted.status, TaskStatus::Merged);
    }
}
