//! Project menu, persisted task lists, and settings editor (§5.5, §8).

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Padding, Paragraph, Wrap};
use ratatui::Frame;

use reporch_core::{CarryMode, ProjectBase};

use crate::project::catalog::{TaskCatalog, TaskRecord};

use super::theme;
use super::wizard::AnalysisState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectMenuItem {
    NewTask,
    Running,
    Completed,
    History,
    Settings,
}

impl ProjectMenuItem {
    pub const ALL: [Self; 5] = [
        Self::NewTask,
        Self::Running,
        Self::Completed,
        Self::History,
        Self::Settings,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskListKind {
    Running,
    Completed,
    History,
}

impl TaskListKind {
    pub const fn title(self) -> &'static str {
        match self {
            Self::Running => "running tasks",
            Self::Completed => "completed tasks",
            Self::History => "task history",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SettingsState {
    pub cursor: usize,
    pub input: Option<String>,
    pub confirm_drop: Option<String>,
    pub analysis: Vec<(String, AnalysisState)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsRow {
    Reanalyse,
    Budget,
    Carry(usize),
    AddCarry,
    Repo(usize),
}

impl SettingsState {
    pub fn rows(&self, project: &ProjectBase) -> Vec<SettingsRow> {
        std::iter::once(SettingsRow::Reanalyse)
            .chain(std::iter::once(SettingsRow::Budget))
            .chain(
                project
                    .settings
                    .worktree_carry_over
                    .iter()
                    .enumerate()
                    .map(|(index, _)| SettingsRow::Carry(index)),
            )
            .chain(std::iter::once(SettingsRow::AddCarry))
            .chain(
                project
                    .repositories
                    .iter()
                    .enumerate()
                    .map(|(index, _)| SettingsRow::Repo(index)),
            )
            .collect()
    }

    pub fn selected(&self, project: &ProjectBase) -> SettingsRow {
        let rows = self.rows(project);
        rows[self.cursor.min(rows.len().saturating_sub(1))]
    }

    pub fn clamp(&mut self, project: &ProjectBase) {
        self.cursor = self.cursor.min(self.rows(project).len().saturating_sub(1));
    }
}

pub fn filtered_tasks(catalog: &TaskCatalog, kind: TaskListKind) -> Vec<&TaskRecord> {
    match kind {
        TaskListKind::Running => catalog.running().collect(),
        TaskListKind::Completed => catalog.completed().collect(),
        TaskListKind::History => catalog.all_finished().collect(),
    }
}

pub fn draw_project(
    frame: &mut Frame,
    area: Rect,
    project: &ProjectBase,
    catalog: &TaskCatalog,
    cursor: usize,
) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("  "),
            Span::raw(project.name.clone()).bold().fg(theme::ACCENT),
            Span::raw(format!(
                "    {} repos · {} tasks",
                project.repositories.len(),
                catalog.tasks.len()
            ))
            .fg(theme::DIM),
        ])),
        header,
    );
    let running = catalog.running().count();
    let completed = catalog.completed().count();
    let labels = [
        "new task".into(),
        format!("running tasks          {running}"),
        format!("completed tasks        {completed}"),
        "task history".into(),
        "project settings".into(),
    ];
    let mut state = ListState::default();
    state.select(Some(cursor.min(labels.len() - 1)));
    frame.render_stateful_widget(
        List::new(labels.into_iter().map(ListItem::new).collect::<Vec<_>>())
            .block(Block::default().padding(Padding::new(2, 2, 1, 0)))
            .highlight_symbol("› ")
            .highlight_style(theme::selected()),
        body,
        &mut state,
    );
    frame.render_widget(
        Paragraph::new(
            Line::from(Span::raw("  ↑↓ move · enter select · esc projects")).fg(theme::DIM),
        ),
        footer,
    );
}

pub fn draw_tasks(
    frame: &mut Frame,
    area: Rect,
    project: &ProjectBase,
    catalog: &TaskCatalog,
    kind: TaskListKind,
    cursor: usize,
) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(area);
    let tasks = filtered_tasks(catalog, kind);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("  "),
            Span::raw(format!("{} · {}", project.name, kind.title()))
                .bold()
                .fg(theme::ACCENT),
        ])),
        header,
    );
    let items = if tasks.is_empty() {
        vec![ListItem::new("No tasks in this view.").style(theme::DIM)]
    } else {
        tasks
            .iter()
            .map(|record| {
                ListItem::new(format!(
                    "{:<11} {:<10} {:<38} ${:.2}",
                    record.task.id,
                    record.task.status,
                    reporch_claude::events::truncate_chars(&record.task.name, 36),
                    record.task.spent_usd
                ))
            })
            .collect()
    };
    let mut state = ListState::default();
    state.select((!tasks.is_empty()).then_some(cursor.min(tasks.len() - 1)));
    frame.render_stateful_widget(
        List::new(items)
            .block(Block::default().padding(Padding::new(2, 2, 1, 0)))
            .highlight_symbol("› ")
            .highlight_style(theme::selected()),
        body,
        &mut state,
    );
    let hint = if kind == TaskListKind::Completed {
        "  ↑↓ move · enter review merge · esc project"
    } else {
        "  ↑↓ move · enter inspect · esc project"
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::raw(hint)).fg(theme::DIM)),
        footer,
    );
}

pub fn draw_task(frame: &mut Frame, area: Rect, record: &TaskRecord) {
    let text = task_text(record);
    frame.render_widget(
        Paragraph::new(text)
            .block(Block::default().padding(Padding::new(2, 2, 1, 1)))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn task_text(record: &TaskRecord) -> String {
    let base = reporch_store::atomic::read_string(&record.paths.base_md()).unwrap_or_default();
    let result = reporch_store::atomic::read_string(&record.paths.result_md()).unwrap_or_default();
    let tokens = record.task.total_token_usage();
    let mut text = format!(
        "{}  {}\n\nstatus      {}\nmaster      {}\nmodel       {}\nspend       ${:.2} / ${:.2}\ntokens      {} input · {} output · {} cache read · {} cache write\nbranch      {}\nartifacts   {}\n\n## Repository usage\n",
        record.task.id,
        record.task.name,
        record.task.status,
        record.task.master_repo,
        record.task.model,
        record.task.spent_usd,
        record.task.budget_usd,
        tokens.input,
        tokens.output,
        tokens.cache_read,
        tokens.cache_creation,
        record.task.branch,
        record.paths.dir().display(),
    );
    for repo in &record.task.repositories {
        text.push_str(&format!(
            "\n- {} · {} · {} · ${:.4} · {} in / {} out / {} cache read / {} cache write",
            repo.name,
            repo.role,
            repo.state,
            repo.spent_usd,
            repo.token_usage.input,
            repo.token_usage.output,
            repo.token_usage.cache_read,
            repo.token_usage.cache_creation,
        ));
    }
    text.push_str("\n\n## Brief\n\n");
    text.push_str(&base);
    if !result.is_empty() {
        text.push_str("\n\n## Result\n\n");
        text.push_str(&result);
    }
    text
}

pub fn draw_settings(frame: &mut Frame, area: Rect, project: &ProjectBase, state: &SettingsState) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(2),
    ])
    .areas(area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("  "),
            Span::raw(format!("{} · project settings", project.name))
                .bold()
                .fg(theme::ACCENT),
        ])),
        header,
    );
    if state.analysis.is_empty() {
        let rows = state.rows(project);
        let items = rows
            .iter()
            .map(|row| {
                let label = match *row {
                    SettingsRow::Reanalyse => "re-run repository analysis".into(),
                    SettingsRow::Budget => format!(
                        "session budget ceiling       ${:.2}",
                        project.settings.max_budget_usd_per_session
                    ),
                    SettingsRow::Carry(index) => {
                        let carry = &project.settings.worktree_carry_over[index];
                        format!(
                            "carry into worktrees        {}  [{:?}]",
                            carry.pattern, carry.mode
                        )
                    }
                    SettingsRow::AddCarry => "+ add carry-over path".into(),
                    SettingsRow::Repo(index) => {
                        format!(
                            "repository                  {}",
                            project.repositories[index].name
                        )
                    }
                };
                ListItem::new(label)
            })
            .collect::<Vec<_>>();
        let mut list_state = ListState::default();
        list_state.select(Some(state.cursor.min(items.len().saturating_sub(1))));
        frame.render_stateful_widget(
            List::new(items)
                .block(Block::default().padding(Padding::new(2, 2, 1, 0)))
                .highlight_symbol("› ")
                .highlight_style(theme::selected()),
            body,
            &mut list_state,
        );
    } else {
        // Re-analysis is paid, long-running work. Show the same real per-repository progress as
        // initial analysis rather than replacing it with an indeterminate spinner (§5.4).
        let lines = state
            .analysis
            .iter()
            .map(|(name, progress)| match progress {
                AnalysisState::Waiting => Line::from(vec![
                    Span::raw(format!("  {name:<24}")),
                    Span::raw("waiting").fg(theme::DIM),
                ]),
                AnalysisState::Running { elapsed_secs } => Line::from(vec![
                    Span::raw(format!("  {name:<24}")),
                    Span::raw(format!("reading… {elapsed_secs}s")).fg(theme::ACCENT),
                ]),
                AnalysisState::Done { one_liner } => Line::from(vec![
                    Span::raw(format!("  {name:<24}")),
                    Span::raw(reporch_claude::events::truncate_chars(one_liner, 48)).fg(theme::OK),
                ]),
                AnalysisState::Failed(error) => Line::from(vec![
                    Span::raw(format!("  {name:<24}")),
                    Span::raw(reporch_claude::events::truncate_chars(error, 48)).fg(theme::FAIL),
                ]),
            })
            .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(lines).block(Block::default().padding(Padding::new(2, 2, 1, 0))),
            body,
        );
    }

    let hint = if let Some(input) = &state.input {
        format!("  carry-over path: {input}█ · enter add · esc cancel")
    } else if let Some(repo) = &state.confirm_drop {
        format!("  Drop `{repo}` from this project? y confirm · n/esc cancel")
    } else if !state.analysis.is_empty() {
        let done = state
            .analysis
            .iter()
            .filter(|(_, state)| {
                matches!(state, AnalysisState::Done { .. } | AnalysisState::Failed(_))
            })
            .count();
        format!("  analysing {done}/{} repositories…", state.analysis.len())
    } else {
        "  ↑↓ move · ←→ budget · m carry mode · d remove · enter action · esc project".into()
    };
    frame.render_widget(
        Paragraph::new(hint).style(if state.confirm_drop.is_some() {
            ratatui::style::Style::default().fg(theme::WARN)
        } else {
            ratatui::style::Style::default().fg(theme::DIM)
        }),
        footer,
    );
}

pub const fn next_mode(mode: CarryMode) -> CarryMode {
    match mode {
        CarryMode::Copy => CarryMode::CopyOnWrite,
        CarryMode::CopyOnWrite => CarryMode::Symlink,
        CarryMode::Symlink => CarryMode::Copy,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reporch_core::{Effort, RepoState, Role, Task, TaskRepo, TaskStatus, TokenUsage};

    #[test]
    fn completed_task_text_keeps_the_per_repo_cost_and_token_breakdown() {
        let temp = tempfile::tempdir().unwrap();
        let paths = reporch_store::TaskPaths::at(temp.path().join("task"));
        reporch_store::atomic::write_string(&paths.base_md(), "Do the thing.").unwrap();
        reporch_store::atomic::write_string(&paths.result_md(), "Thing done.").unwrap();
        let usage = TokenUsage {
            input: 13,
            output: 570,
            cache_read: 215_881,
            cache_creation: 54_927,
        };
        let record = TaskRecord {
            paths,
            task: Task {
                schema_version: 1,
                id: "TASK-0001".parse().unwrap(),
                name: "usage-proof".into(),
                status: TaskStatus::Completed,
                created_at: "2026-08-12T00:00:00Z".into(),
                started_at: None,
                finished_at: None,
                master_repo: "api".into(),
                model: "sonnet".into(),
                master_effort: Effort::High,
                worker_effort: Effort::Medium,
                ultracode: false,
                budget_usd: 1.0,
                spent_usd: 0.4025,
                branch: "reporch/TASK-0001-usage-proof".into(),
                repositories: vec![TaskRepo {
                    name: "api".into(),
                    path: "/repo/api".into(),
                    role: Role::Master,
                    worktree_path: "/worktree/api".into(),
                    branch: "reporch/TASK-0001-usage-proof".into(),
                    base_commit: "abc".into(),
                    session_alias: "api@TASK-0001".into(),
                    session_id: "session".into(),
                    session_pid: None,
                    model: "sonnet".into(),
                    effort: Effort::High,
                    ultracode: false,
                    state: RepoState::Done,
                    changed: Some(true),
                    last_activity_at: None,
                    spent_usd: 0.4025,
                    token_usage: usage,
                }],
            },
        };

        let text = task_text(&record);
        assert!(text.contains("13 input · 570 output"), "{text}");
        assert!(text.contains("api · master · done · $0.4025"), "{text}");
        assert!(
            text.contains("215881 cache read / 54927 cache write"),
            "{text}"
        );
        assert!(text.contains("## Result\n\nThing done."), "{text}");
    }
}
