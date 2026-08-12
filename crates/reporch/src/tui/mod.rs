//! The terminal console.
//!
//! ratatui is immediate mode: the whole UI is redrawn every frame, state lives outside the
//! widgets, and the event loop is ours (`docs/ratatui-notes.md`). So there is no widget tree here
//! — there is an [`App`] value, and a `draw` function that reads it.

pub mod flow;
pub mod menu;
pub mod merge_view;
pub mod project_views;
pub mod run;
pub mod task_views;
pub mod theme;
pub mod wizard;

use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;

use reporch_store::{Config, Paths};

use crate::task;

use self::flow::Command as FlowCommand;
use self::menu::{MainMenu, MenuItem};
use self::wizard::Wizard;

/// Long-running work requested by a key press. The immediate-mode [`App`] only describes the
/// effect; the Tokio-backed console loop owns and supervises the task.
#[derive(Debug)]
enum Effect {
    StartAnalysis(Box<flow::AnalysisRequest>),
    StartTask(Box<task::runtime::TaskRunRequest>),
    SetTaskPaused(bool),
    AbortTask,
    LoadMerge {
        record: Box<crate::project::catalog::TaskRecord>,
        project: Box<reporch_core::ProjectBase>,
    },
    Merge(merge_view::MergeAction),
    CheckUpdates,
    OpenRepository,
}

#[derive(Debug)]
enum MergeEvent {
    Loaded(Result<merge_view::PreparedMerge, String>),
    Updated(merge_view::MergeUpdate),
}

#[derive(Debug)]
enum SystemEvent {
    Update(crate::update::UpdateStatus),
    RepositoryOpened(Result<(), String>),
}

/// Which screen is showing. §5 has several; M1 ships the top-level menu and grows from here.
#[derive(Debug, Clone, PartialEq)]
pub enum Screen {
    Main,
    Help,
    /// A `new project` or `new task` flow (§5.2–§5.7).
    Flow,
    /// The list of projects (§5.1 `list projects`).
    Projects,
    /// One project's §5.5 menu.
    Project,
    /// A persisted task bucket under the selected project.
    TaskList(project_views::TaskListKind),
    /// A read-only historical task view.
    TaskDetail,
    /// Editable per-project settings (§5.5 addition).
    Settings,
    /// Per-repository, explicitly confirmed completion review (§11.2).
    Merge,
    /// A live task supervised by the background task actor (§5.8).
    Run,
    /// One selected session's persisted stream, refreshed while it runs (§5.8, §10).
    SessionLog {
        repo: String,
    },
    /// Every brief and ping in append order (§5.8, §8.3).
    Mailbox,
    /// All persisted session streams grouped by repository (§5.8, §10).
    TaskLogs,
    /// A message with nowhere better to go yet — used for the not-yet-built screens so the menu
    /// is honest about what exists.
    Notice {
        title: String,
        body: String,
    },
}

/// Everything the UI draws from. Widgets hold nothing.
#[derive(Debug)]
pub struct App {
    pub screen: Screen,
    pub main_menu: MainMenu,
    pub should_quit: bool,
    pub paths: Paths,
    pub config: Config,
    /// A transient line at the bottom of the screen.
    pub status: Option<String>,
    /// The active `new project` / `new task` flow, if any.
    pub wizard: Option<Wizard>,
    /// Shared selection state for whichever list the flow is showing. Immediate mode means the
    /// widget cannot hold it (`docs/ratatui-notes.md` §4).
    pub list_state: ratatui::widgets::ListState,
    /// The project the current task flow belongs to.
    pub project: Option<reporch_core::ProjectBase>,
    pub projects: Vec<String>,
    pub projects_cursor: usize,
    pub project_menu_cursor: usize,
    pub task_catalog: crate::project::catalog::TaskCatalog,
    pub task_cursor: usize,
    pub selected_task: Option<crate::project::catalog::TaskRecord>,
    pub settings: project_views::SettingsState,
    pub active_merge: Option<merge_view::MergeState>,
    analysis_started_at: Option<Instant>,
    pub active_run: Option<ActiveRun>,
    pub run_table: ratatui::widgets::TableState,
    pub task_view_cursor: usize,
    pub task_view_detail_scroll: u16,
    task_commands: Option<tokio::sync::mpsc::UnboundedSender<task::runtime::TaskRunCommand>>,
}

#[derive(Debug)]
pub struct ActiveRun {
    pub task: reporch_core::Task,
    pub paths: reporch_store::TaskPaths,
    pub started_at: Instant,
    pub view: task::RunView,
    pub confirm_abort: bool,
}

impl App {
    pub fn new(paths: Paths, config: Config) -> Self {
        Self {
            screen: Screen::Main,
            main_menu: MainMenu::new(),
            should_quit: false,
            paths,
            config,
            status: None,
            wizard: None,
            list_state: ratatui::widgets::ListState::default(),
            project: None,
            projects: Vec::new(),
            projects_cursor: 0,
            project_menu_cursor: 0,
            task_catalog: crate::project::catalog::TaskCatalog::default(),
            task_cursor: 0,
            selected_task: None,
            settings: project_views::SettingsState::default(),
            active_merge: None,
            analysis_started_at: None,
            active_run: None,
            run_table: ratatui::widgets::TableState::default(),
            task_view_cursor: 0,
            task_view_detail_scroll: 0,
            task_commands: None,
        }
    }

    /// Load a project's `project_base.json`.
    fn load_project(&self, name: &str) -> Option<reporch_core::ProjectBase> {
        reporch_store::atomic::read_json_opt(&self.paths.project_base_json(name))
            .ok()
            .flatten()
    }

    fn on_key(&mut self, key: KeyEvent) -> Option<Effect> {
        // Ctrl-C always exits, from anywhere. A tool that traps the user in a screen is a tool
        // they stop trusting.
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
        {
            self.should_quit = true;
            return self.task_commands.as_ref().map(|_| Effect::AbortTask);
        }
        self.status = None;
        match self.screen.clone() {
            Screen::Main => return self.on_key_main(key),
            Screen::Flow => return self.on_key_flow(key),
            Screen::Projects => self.on_key_projects(key),
            Screen::Project => self.on_key_project(key),
            Screen::TaskList(kind) => return self.on_key_task_list(key, kind),
            Screen::TaskDetail => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => self.screen = Screen::Project,
                _ => {}
            },
            Screen::Settings => return self.on_key_settings(key),
            Screen::Merge => return self.on_key_merge(key),
            Screen::Run => return self.on_key_run(key),
            Screen::SessionLog { .. } | Screen::Mailbox | Screen::TaskLogs => {
                self.on_key_task_view(key)
            }
            Screen::Help | Screen::Notice { .. } => match key.code {
                KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('q') | KeyCode::Enter => {
                    self.screen = Screen::Main;
                }
                _ => {}
            },
        }
        None
    }

    fn on_key_flow(&mut self, key: KeyEvent) -> Option<Effect> {
        let Some(w) = self.wizard.as_mut() else {
            self.screen = Screen::Main;
            return None;
        };
        match flow::on_key(w, key, self.project.as_ref()) {
            FlowCommand::None => {}
            FlowCommand::Cancel => {
                self.wizard = None;
                self.project = None;
                self.screen = Screen::Main;
            }
            // Scanning is synchronous and measured at well under a second (§5.3 Deviation), so
            // it runs inline rather than behind a spinner the user would see for one frame.
            FlowCommand::Scan => {
                flow::do_scan(w, &self.config);
                self.status = Some(format!(
                    "{} repositories in {:.2}s",
                    w.candidates.len(),
                    w.scan_secs
                ));
            }
            FlowCommand::ConsentGiven => {
                let selected = w.selected_repos();
                w.analysis = selected
                    .iter()
                    .map(|repo| (repo.name.clone(), wizard::AnalysisState::Waiting))
                    .collect();
                self.analysis_started_at = Some(Instant::now());
                return Some(Effect::StartAnalysis(Box::new(flow::AnalysisRequest {
                    project_name: w.project_name.clone(),
                    selected,
                    paths: self.paths.clone(),
                    config: self.config.clone(),
                    preserve: None,
                })));
            }
            FlowCommand::Analyse => {}
            FlowCommand::StartTask => {
                let Some(project) = self.project.clone() else {
                    w.step = wizard::Step::Failed("the project profile is no longer loaded".into());
                    return None;
                };
                let create = task::CreateRequest {
                    project: project.name.clone(),
                    brief: w.brief.clone(),
                    slug: reporch_core::slugify(&w.brief).unwrap_or_else(|| "task".into()),
                    model: w.model().to_string(),
                    effort: w.effort_plan(),
                    master: w.master.clone(),
                    budget_usd_per_session: project.settings.max_budget_usd_per_session,
                };
                return Some(Effect::StartTask(Box::new(task::runtime::TaskRunRequest {
                    paths: self.paths.clone(),
                    config: self.config.clone(),
                    project,
                    create,
                })));
            }
        }
        None
    }

    fn on_key_projects(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.screen = Screen::Main,
            KeyCode::Down | KeyCode::Char('j') if !self.projects.is_empty() => {
                self.projects_cursor = (self.projects_cursor + 1) % self.projects.len();
            }
            KeyCode::Up | KeyCode::Char('k') if !self.projects.is_empty() => {
                self.projects_cursor =
                    (self.projects_cursor + self.projects.len() - 1) % self.projects.len();
            }
            KeyCode::Enter => {
                let Some(name) = self.projects.get(self.projects_cursor).cloned() else {
                    return;
                };
                match self.load_project(&name) {
                    Some(p) => {
                        self.task_catalog =
                            crate::project::catalog::load_tasks(&self.paths, &p.name);
                        self.status = (!self.task_catalog.warnings.is_empty())
                            .then(|| self.task_catalog.warnings.join(" · "));
                        self.project = Some(p);
                        self.project_menu_cursor = 0;
                        self.screen = Screen::Project;
                    }
                    None => {
                        self.screen = Screen::Notice {
                            title: name.clone(),
                            body: format!(
                            "`{name}` has no project_base.json, so it has not been analysed.\n\n\
                                 Create it again with `new project`."
                        ),
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn on_key_project(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.screen = Screen::Projects,
            KeyCode::Down | KeyCode::Char('j') => {
                self.project_menu_cursor =
                    (self.project_menu_cursor + 1) % project_views::ProjectMenuItem::ALL.len();
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let count = project_views::ProjectMenuItem::ALL.len();
                self.project_menu_cursor = (self.project_menu_cursor + count - 1) % count;
            }
            KeyCode::Enter => match project_views::ProjectMenuItem::ALL[self
                .project_menu_cursor
                .min(project_views::ProjectMenuItem::ALL.len() - 1)]
            {
                project_views::ProjectMenuItem::NewTask => {
                    let Some(project) = self.project.clone() else {
                        return;
                    };
                    self.wizard = Some(Wizard::for_task(&project));
                    self.screen = Screen::Flow;
                }
                project_views::ProjectMenuItem::Running => {
                    self.task_cursor = 0;
                    self.screen = Screen::TaskList(project_views::TaskListKind::Running);
                }
                project_views::ProjectMenuItem::Completed => {
                    self.task_cursor = 0;
                    self.screen = Screen::TaskList(project_views::TaskListKind::Completed);
                }
                project_views::ProjectMenuItem::History => {
                    self.task_cursor = 0;
                    self.screen = Screen::TaskList(project_views::TaskListKind::History);
                }
                project_views::ProjectMenuItem::Settings => {
                    self.settings = project_views::SettingsState::default();
                    self.screen = Screen::Settings;
                }
            },
            _ => {}
        }
    }

    fn on_key_task_list(
        &mut self,
        key: KeyEvent,
        kind: project_views::TaskListKind,
    ) -> Option<Effect> {
        let count = project_views::filtered_tasks(&self.task_catalog, kind).len();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.screen = Screen::Project,
            KeyCode::Down | KeyCode::Char('j') if count > 0 => {
                self.task_cursor = (self.task_cursor + 1) % count;
            }
            KeyCode::Up | KeyCode::Char('k') if count > 0 => {
                self.task_cursor = (self.task_cursor + count - 1) % count;
            }
            KeyCode::Enter if count > 0 => {
                self.selected_task = project_views::filtered_tasks(&self.task_catalog, kind)
                    .get(self.task_cursor)
                    .map(|record| (*record).clone());
                if kind == project_views::TaskListKind::Completed {
                    let (Some(record), Some(project)) =
                        (self.selected_task.clone(), self.project.clone())
                    else {
                        return None;
                    };
                    self.active_merge = Some(merge_view::MergeState::loading(&record));
                    self.screen = Screen::Merge;
                    return Some(Effect::LoadMerge {
                        record: Box::new(record),
                        project: Box::new(project),
                    });
                } else {
                    self.screen = Screen::TaskDetail;
                }
            }
            _ => {}
        }
        None
    }

    fn on_key_merge(&mut self, key: KeyEvent) -> Option<Effect> {
        let Some(state) = self.active_merge.as_mut() else {
            self.screen = Screen::TaskList(project_views::TaskListKind::Completed);
            return None;
        };
        if state.loading || state.busy {
            return None;
        }
        if state.confirm_skip {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    state.confirm_skip = false;
                    state.busy = true;
                    return Some(Effect::Merge(merge_view::MergeAction::Skip));
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    state.confirm_skip = false;
                }
                _ => {}
            }
            return None;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.active_merge = None;
                self.task_catalog = self
                    .project
                    .as_ref()
                    .map(|project| crate::project::catalog::load_tasks(&self.paths, &project.name))
                    .unwrap_or_default();
                self.screen = Screen::TaskList(project_views::TaskListKind::Completed);
            }
            KeyCode::Char('r')
                if state.recovery_error.is_some()
                    || state.plan.as_ref().is_some_and(task::MergePlan::is_done) =>
            {
                state.busy = true;
                return Some(Effect::Merge(merge_view::MergeAction::Finalize));
            }
            // The capability proof is constructed at exactly one boundary: a literal key event.
            // No startup, retry timer or background job can call `Confirmation::by_user()`.
            KeyCode::Char('y') | KeyCode::Char('Y')
                if state.recovery_error.is_none()
                    && !state.complete
                    && state.plan.as_ref().is_some_and(|plan| !plan.is_done()) =>
            {
                state.busy = true;
                return Some(Effect::Merge(merge_view::MergeAction::Confirm(
                    task::Confirmation::by_user(),
                )));
            }
            KeyCode::Char('s')
                if state.recovery_error.is_none()
                    && !state.complete
                    && state.plan.as_ref().is_some_and(|plan| !plan.is_done()) =>
            {
                state.confirm_skip = true;
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::PageDown => {
                state.detail_scroll =
                    state
                        .detail_scroll
                        .saturating_add(if matches!(key.code, KeyCode::PageDown) {
                            8
                        } else {
                            1
                        });
            }
            KeyCode::Up | KeyCode::Char('k') | KeyCode::PageUp => {
                state.detail_scroll =
                    state
                        .detail_scroll
                        .saturating_sub(if matches!(key.code, KeyCode::PageUp) {
                            8
                        } else {
                            1
                        });
            }
            _ => {}
        }
        None
    }

    fn on_key_settings(&mut self, key: KeyEvent) -> Option<Effect> {
        if let Some(input) = self.settings.input.as_mut() {
            match key.code {
                KeyCode::Esc => self.settings.input = None,
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Enter => {
                    let pattern = input.trim().to_string();
                    self.settings.input = None;
                    if !valid_carry_path(&pattern) {
                        self.status = Some(
                            "carry-over paths must be non-empty relative paths without `..`".into(),
                        );
                    } else if let Some(project) = self.project.as_mut() {
                        if !project
                            .settings
                            .worktree_carry_over
                            .iter()
                            .any(|carry| carry.pattern == pattern)
                        {
                            project
                                .settings
                                .worktree_carry_over
                                .push(reporch_core::CarryOver {
                                    pattern,
                                    mode: reporch_core::CarryMode::Copy,
                                });
                            self.persist_project();
                        }
                    }
                }
                KeyCode::Char(character) => input.push(character),
                _ => {}
            }
            return None;
        }

        if let Some(repo) = self.settings.confirm_drop.clone() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.settings.confirm_drop = None;
                    if let Some(project) = self.project.as_mut() {
                        if project.repositories.len() <= 1 {
                            self.status =
                                Some("a project must keep at least one repository".into());
                            return None;
                        }
                        project
                            .repositories
                            .retain(|candidate| candidate.name != repo);
                        project.graph.remove(&repo);
                        for node in project.graph.values_mut() {
                            node.consumes.retain(|name| name != &repo);
                            node.produces_contracts_for.retain(|name| name != &repo);
                        }
                        project.settings.post_create_hooks.remove(&repo);
                    }
                    self.settings.clamp(
                        self.project
                            .as_ref()
                            .expect("project settings need a project"),
                    );
                    self.persist_project();
                    self.status = Some(format!("dropped `{repo}` from this project"));
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.settings.confirm_drop = None;
                }
                _ => {}
            }
            return None;
        }

        let Some(project) = self.project.as_ref() else {
            self.screen = Screen::Projects;
            return None;
        };
        let rows = self.settings.rows(project);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.screen = Screen::Project,
            KeyCode::Down | KeyCode::Char('j') => {
                self.settings.cursor = (self.settings.cursor + 1) % rows.len();
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.settings.cursor = (self.settings.cursor + rows.len() - 1) % rows.len();
            }
            KeyCode::Left | KeyCode::Right
                if self.settings.selected(project) == project_views::SettingsRow::Budget =>
            {
                let delta = if key.code == KeyCode::Right {
                    1.0
                } else {
                    -1.0
                };
                if let Some(project) = self.project.as_mut() {
                    project.settings.max_budget_usd_per_session =
                        (project.settings.max_budget_usd_per_session + delta).max(0.05);
                }
                self.persist_project();
            }
            KeyCode::Char('m') => {
                if let project_views::SettingsRow::Carry(index) = self.settings.selected(project) {
                    if let Some(project) = self.project.as_mut() {
                        let carry = &mut project.settings.worktree_carry_over[index];
                        carry.mode = project_views::next_mode(carry.mode);
                    }
                    self.persist_project();
                }
            }
            KeyCode::Char('d') => match self.settings.selected(project) {
                project_views::SettingsRow::Carry(index) => {
                    if let Some(project) = self.project.as_mut() {
                        project.settings.worktree_carry_over.remove(index);
                    }
                    self.settings
                        .clamp(self.project.as_ref().expect("project exists"));
                    self.persist_project();
                }
                project_views::SettingsRow::Repo(index) => {
                    self.settings.confirm_drop = self
                        .project
                        .as_ref()
                        .map(|project| project.repositories[index].name.clone());
                }
                _ => {}
            },
            KeyCode::Enter => match self.settings.selected(project) {
                project_views::SettingsRow::Reanalyse => {
                    let project = project.clone();
                    let selected = project
                        .repositories
                        .iter()
                        .map(|repo| reporch_scan::FoundRepo {
                            name: repo.name.clone(),
                            path: std::path::PathBuf::from(&repo.path),
                            remote_url: repo.remote_url.clone(),
                            remote_org: None,
                            head_age_secs: None,
                        })
                        .collect::<Vec<_>>();
                    self.settings.analysis = selected
                        .iter()
                        .map(|repo| (repo.name.clone(), wizard::AnalysisState::Waiting))
                        .collect();
                    self.analysis_started_at = Some(Instant::now());
                    return Some(Effect::StartAnalysis(Box::new(flow::AnalysisRequest {
                        project_name: project.name.clone(),
                        selected,
                        paths: self.paths.clone(),
                        config: self.config.clone(),
                        preserve: Some(project),
                    })));
                }
                project_views::SettingsRow::AddCarry => {
                    self.settings.input = Some(String::new());
                }
                _ => {}
            },
            _ => {}
        }
        None
    }

    fn persist_project(&mut self) {
        let Some(project) = self.project.as_ref() else {
            return;
        };
        if let Err(error) =
            reporch_store::atomic::write_json(&self.paths.project_base_json(&project.name), project)
        {
            self.status = Some(format!("could not save project settings: {error}"));
            return;
        }
        let mut profiles = std::collections::BTreeMap::new();
        for repo in &project.repositories {
            if let Ok(Some(profile)) =
                reporch_store::atomic::read_json_opt::<reporch_core::RepoProfile>(
                    &self.paths.repo_profile_json(&project.name, &repo.name),
                )
            {
                profiles.insert(repo.name.clone(), profile);
            }
        }
        if let Err(error) = reporch_store::atomic::write_string(
            &self.paths.project_base_md(&project.name),
            &crate::project::render_markdown(project, &profiles),
        ) {
            self.status = Some(format!(
                "saved JSON, but could not refresh project_base.md: {error}"
            ));
        }
    }

    fn on_key_main(&mut self, key: KeyEvent) -> Option<Effect> {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Down | KeyCode::Char('j') => self.main_menu.next(),
            KeyCode::Up | KeyCode::Char('k') => self.main_menu.previous(),
            KeyCode::Home | KeyCode::Char('g') => self.main_menu.select(0),
            KeyCode::End | KeyCode::Char('G') => self.main_menu.select(MenuItem::ALL.len() - 1),
            KeyCode::Enter => return self.activate(),
            _ => {}
        }
        None
    }

    fn on_key_run(&mut self, key: KeyEvent) -> Option<Effect> {
        if matches!(key.code, KeyCode::Esc | KeyCode::Char('q'))
            && self.task_commands.is_none()
            && self
                .active_run
                .as_ref()
                .is_some_and(|active| active.view.finished.is_some())
        {
            if let Some(project) = &self.project {
                self.task_catalog = crate::project::catalog::load_tasks(&self.paths, &project.name);
            }
            self.project_menu_cursor = project_views::ProjectMenuItem::ALL
                .iter()
                .position(|item| *item == project_views::ProjectMenuItem::Completed)
                .unwrap_or(0);
            self.selected_task = None;
            self.active_run = None;
            self.screen = Screen::Project;
            return None;
        }

        if self
            .active_run
            .as_ref()
            .is_some_and(|active| active.confirm_abort)
        {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    if let Some(active) = self.active_run.as_mut() {
                        active.confirm_abort = false;
                    }
                    return Some(Effect::AbortTask);
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    if let Some(active) = self.active_run.as_mut() {
                        active.confirm_abort = false;
                    }
                }
                _ => {}
            }
            return None;
        }

        let rows = self
            .active_run
            .as_ref()
            .map_or(0, |active| active.view.rows.len());
        match key.code {
            KeyCode::Down | KeyCode::Char('j') if rows > 0 => {
                let next = (self.run_table.selected().unwrap_or(0) + 1) % rows;
                self.run_table.select(Some(next));
            }
            KeyCode::Up | KeyCode::Char('k') if rows > 0 => {
                let current = self.run_table.selected().unwrap_or(0);
                self.run_table.select(Some((current + rows - 1) % rows));
            }
            KeyCode::Enter if rows > 0 => {
                if let Some(repo) = self
                    .active_run
                    .as_ref()
                    .and_then(|active| active.view.rows.get(self.run_table.selected().unwrap_or(0)))
                    .map(|row| row.repo.clone())
                {
                    self.open_task_view(Screen::SessionLog { repo });
                }
            }
            KeyCode::Char('b') => self.open_task_view(Screen::Mailbox),
            KeyCode::Char('l') => self.open_task_view(Screen::TaskLogs),
            KeyCode::Char('p') => {
                let paused = self
                    .active_run
                    .as_ref()
                    .is_some_and(|active| active.view.paused);
                return Some(Effect::SetTaskPaused(!paused));
            }
            KeyCode::Char('x') => {
                if let Some(active) = self.active_run.as_mut() {
                    active.confirm_abort = true;
                }
            }
            _ => {}
        }
        None
    }

    fn open_task_view(&mut self, screen: Screen) {
        self.task_view_cursor = 0;
        self.task_view_detail_scroll = 0;
        self.screen = screen;
    }

    fn on_key_task_view(&mut self, key: KeyEvent) {
        let count = self.task_diagnostic_entries().len();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.screen = Screen::Run,
            KeyCode::Char('b') => self.open_task_view(Screen::Mailbox),
            KeyCode::Char('l') => self.open_task_view(Screen::TaskLogs),
            KeyCode::Down | KeyCode::Char('j') if count > 0 => {
                self.task_view_cursor = (self.task_view_cursor + 1) % count;
                self.task_view_detail_scroll = 0;
            }
            KeyCode::Up | KeyCode::Char('k') if count > 0 => {
                self.task_view_cursor = (self.task_view_cursor + count - 1) % count;
                self.task_view_detail_scroll = 0;
            }
            KeyCode::PageDown => {
                self.task_view_detail_scroll = self.task_view_detail_scroll.saturating_add(10);
            }
            KeyCode::PageUp => {
                self.task_view_detail_scroll = self.task_view_detail_scroll.saturating_sub(10);
            }
            KeyCode::Home => {
                self.task_view_cursor = 0;
                self.task_view_detail_scroll = 0;
            }
            KeyCode::End if count > 0 => {
                self.task_view_cursor = count - 1;
                self.task_view_detail_scroll = 0;
            }
            _ => {}
        }
    }

    pub(crate) fn task_diagnostic_entries(&self) -> Vec<task_views::DiagnosticEntry> {
        let Some(active) = &self.active_run else {
            return Vec::new();
        };
        match &self.screen {
            Screen::SessionLog { repo } => task_views::session_log(&active.paths, repo),
            Screen::Mailbox => task_views::mailbox_timeline(&active.paths),
            Screen::TaskLogs => task_views::task_logs(&active.paths, &active.task),
            _ => Vec::new(),
        }
    }

    fn activate(&mut self) -> Option<Effect> {
        match self.main_menu.selected() {
            MenuItem::NewProject => {
                self.wizard = Some(Wizard::new());
                self.project = None;
                self.screen = Screen::Flow;
            }
            MenuItem::ListProjects => {
                let dir = self.paths.projects_dir();
                self.projects = list_projects(&dir);
                self.projects_cursor = 0;
                self.screen = if self.projects.is_empty() {
                    Screen::Notice {
                        title: "list projects".into(),
                        body: format!(
                            "No projects yet.\n\nreporch keeps them in {}.\n\nStart with \
                             `new project`.",
                            dir.display()
                        ),
                    }
                } else {
                    Screen::Projects
                };
            }
            MenuItem::Help => self.screen = Screen::Help,
            MenuItem::CheckUpdates => {
                self.screen = Screen::Notice {
                    title: "check updates".into(),
                    body: format!(
                        "reporch {}\n\nChecking GitHub releases…",
                        env!("CARGO_PKG_VERSION")
                    ),
                };
                return Some(Effect::CheckUpdates);
            }
            MenuItem::Star => {
                self.status = Some(format!("opening {}", crate::REPO_URL));
                return Some(Effect::OpenRepository);
            }
        }
        None
    }

    fn on_system_event(&mut self, event: SystemEvent) {
        match event {
            SystemEvent::Update(update) => {
                let method = std::env::current_exe()
                    .ok()
                    .as_deref()
                    .map(crate::update::detect_install_method)
                    .unwrap_or(crate::update::InstallMethod::Shell);
                self.screen = Screen::Notice {
                    title: "check updates".into(),
                    body: update.message_for(method),
                };
            }
            SystemEvent::RepositoryOpened(Ok(())) => {
                self.status = Some("opened reporch on GitHub".into());
            }
            SystemEvent::RepositoryOpened(Err(error)) => {
                self.status = Some(format!("could not open GitHub: {error}"));
            }
        }
    }

    /// The periodic tick. The run screen (§5.8) redraws from it.
    fn on_tick(&mut self) {
        let Some(started) = self.analysis_started_at else {
            return;
        };
        let elapsed_secs = started.elapsed().as_secs();
        if let Some(wizard) = self.wizard.as_mut() {
            for (_, state) in &mut wizard.analysis {
                if matches!(state, wizard::AnalysisState::Running { .. }) {
                    *state = wizard::AnalysisState::Running { elapsed_secs };
                }
            }
        }
        for (_, state) in &mut self.settings.analysis {
            if matches!(state, wizard::AnalysisState::Running { .. }) {
                *state = wizard::AnalysisState::Running { elapsed_secs };
            }
        }
    }

    fn on_analysis_event(&mut self, event: flow::AnalysisEvent) {
        match event {
            flow::AnalysisEvent::Running { repo } => {
                let state = wizard::AnalysisState::Running { elapsed_secs: 0 };
                if let Some(wizard) = self.wizard.as_mut() {
                    set_analysis_state(wizard, &repo, state);
                } else {
                    set_analysis_row(&mut self.settings.analysis, &repo, state);
                }
            }
            flow::AnalysisEvent::Done { repo, one_liner } => {
                let state = wizard::AnalysisState::Done { one_liner };
                if let Some(wizard) = self.wizard.as_mut() {
                    set_analysis_state(wizard, &repo, state);
                } else {
                    set_analysis_row(&mut self.settings.analysis, &repo, state);
                }
            }
            flow::AnalysisEvent::Failed { repo, reason } => {
                let state = wizard::AnalysisState::Failed(reason);
                if let Some(wizard) = self.wizard.as_mut() {
                    set_analysis_state(wizard, &repo, state);
                } else {
                    set_analysis_row(&mut self.settings.analysis, &repo, state);
                }
            }
            flow::AnalysisEvent::Finished(result) => {
                self.analysis_started_at = None;
                match *result {
                    Ok(project) => {
                        self.status = Some(format!(
                            "analysed {} repositories for {}",
                            project.repositories.len(),
                            project.name
                        ));
                        self.project = Some(project);
                        if let Some(wizard) = self.wizard.as_mut() {
                            wizard.step = wizard::Step::Done;
                        } else {
                            self.settings.analysis.clear();
                        }
                    }
                    Err(error) => {
                        if let Some(wizard) = self.wizard.as_mut() {
                            wizard.step = wizard::Step::Failed(error);
                        } else {
                            self.settings.analysis.clear();
                            self.status = Some(format!("analysis failed: {error}"));
                        }
                    }
                }
            }
        }
    }

    fn on_task_event(&mut self, event: task::runtime::TaskRunEvent) {
        match event {
            task::runtime::TaskRunEvent::Preparing => {
                self.status = Some("creating worktrees before any session is spawned…".into());
            }
            task::runtime::TaskRunEvent::Started(started) => {
                let started = *started;
                self.status = if started.warnings.is_empty() {
                    None
                } else {
                    Some(started.warnings.join(" · "))
                };
                self.run_table
                    .select((!started.task.repositories.is_empty()).then_some(0));
                self.active_run = Some(ActiveRun {
                    task: started.task,
                    paths: started.paths,
                    started_at: Instant::now(),
                    view: task::RunView::default(),
                    confirm_abort: false,
                });
                self.wizard = None;
                self.screen = Screen::Run;
            }
            task::runtime::TaskRunEvent::View(view) => {
                if let Some(active) = self.active_run.as_mut() {
                    active.view = view;
                    if active.view.finished.is_some() {
                        active.confirm_abort = false;
                    }
                }
            }
            task::runtime::TaskRunEvent::Failed(error) => {
                if self.active_run.is_some() {
                    self.status = Some(error);
                } else {
                    self.screen = Screen::Notice {
                        title: "task startup failed".into(),
                        body: error,
                    };
                    self.wizard = None;
                }
                self.task_commands = None;
            }
            task::runtime::TaskRunEvent::Stopped => {
                self.task_commands = None;
            }
        }
    }

    fn on_merge_event(&mut self, event: MergeEvent) {
        let Some(state) = self.active_merge.as_mut() else {
            return;
        };
        match event {
            MergeEvent::Loaded(Ok(prepared)) => {
                state.loading = false;
                state.current_diff = prepared.current_diff;
                state.report = Some(prepared.report);
                state.plan = Some(prepared.plan);
                state.message = Some("review every changed repository before deciding".into());
                state.detail_scroll = 0;
            }
            MergeEvent::Loaded(Err(error)) => {
                state.loading = false;
                state.recovery_error = Some(format!("could not prepare merge: {error}"));
            }
            MergeEvent::Updated(update) => {
                state.loading = false;
                state.busy = false;
                state.task = update.task;
                state.plan = Some(update.plan);
                state.current_diff = update.current_diff;
                state.message = Some(update.message);
                state.recovery_error = update.recovery_error;
                state.cleanup_problems = update.cleanup_problems;
                state.complete = update.complete;
                state.detail_scroll = 0;
                if state.complete {
                    if let Some(project) = &self.project {
                        self.task_catalog =
                            crate::project::catalog::load_tasks(&self.paths, &project.name);
                    }
                }
            }
        }
    }
}

fn set_analysis_state(wizard: &mut Wizard, repo: &str, state: wizard::AnalysisState) {
    if let Some(slot) = wizard.analysis.iter_mut().find(|(name, _)| name == repo) {
        slot.1 = state;
    }
}

fn set_analysis_row(
    rows: &mut [(String, wizard::AnalysisState)],
    repo: &str,
    state: wizard::AnalysisState,
) {
    if let Some(slot) = rows.iter_mut().find(|(name, _)| name == repo) {
        slot.1 = state;
    }
}

fn valid_carry_path(value: &str) -> bool {
    !value.is_empty()
        && std::path::Path::new(value).is_relative()
        && std::path::Path::new(value)
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

fn list_projects(dir: &std::path::Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

/// Run the console until the user quits.
pub async fn run(paths: Paths, config: Config) -> io::Result<()> {
    // `ratatui::init()` enables raw mode, enters the alternate screen, and installs a panic hook
    // that restores the terminal *before* the default hook prints. Any hook of our own must be
    // installed first, since ratatui chains onto whatever exists when it is called.
    let mut terminal = ratatui::init();
    let result = run_loop(&mut terminal, paths, config).await;
    ratatui::restore();
    result
}

async fn run_loop(terminal: &mut DefaultTerminal, paths: Paths, config: Config) -> io::Result<()> {
    let mut app = App::new(paths, config);
    // Read from the app rather than a second copy of the config, so there is one source of truth.
    // The floor stops a hostile `tick_ms = 0` from turning the poll into a busy-wait.
    let tick = Duration::from_millis(app.config.ui.tick_ms.max(16));
    let mut ticker = tokio::time::interval(tick);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let (analysis_tx, mut analysis_rx) = tokio::sync::mpsc::unbounded_channel();
    let (task_tx, mut task_rx) = tokio::sync::mpsc::unbounded_channel();
    let (merge_tx, mut merge_rx) = tokio::sync::mpsc::unbounded_channel();
    let (system_tx, mut system_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut analysis_jobs = tokio::task::JoinSet::new();
    let mut merge_jobs = tokio::task::JoinSet::new();
    let mut system_jobs = tokio::task::JoinSet::new();
    let mut task_job: Option<tokio::task::JoinHandle<()>> = None;

    while !app.should_quit {
        terminal.draw(|frame| menu::draw(frame, &mut app))?;

        // Crossterm's zero-duration poll is non-blocking. Tokio owns the wait below, so Claude
        // jobs and their progress channels continue even while no key is pressed.
        while event::poll(Duration::ZERO)? {
            match event::read()? {
                // The `Press` guard is not optional: terminals with the Kitty keyboard protocol
                // deliver Release too, and without it every keystroke acts twice.
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if let Some(effect) = app.on_key(key) {
                        match effect {
                            Effect::StartAnalysis(request) => {
                                let events = analysis_tx.clone();
                                analysis_jobs.spawn(flow::run_analysis(*request, events));
                            }
                            Effect::StartTask(request) => {
                                let (commands, command_rx) = tokio::sync::mpsc::unbounded_channel();
                                app.task_commands = Some(commands);
                                let events = task_tx.clone();
                                task_job = Some(tokio::spawn(task::runtime::run_task(
                                    *request, events, command_rx,
                                )));
                            }
                            Effect::SetTaskPaused(paused) => {
                                if let Some(commands) = &app.task_commands {
                                    let _ = commands
                                        .send(task::runtime::TaskRunCommand::SetPaused(paused));
                                }
                            }
                            Effect::AbortTask => {
                                if let Some(commands) = &app.task_commands {
                                    let _ = commands.send(task::runtime::TaskRunCommand::Abort);
                                }
                            }
                            Effect::LoadMerge { record, project } => {
                                let paths = app.paths.clone();
                                let events = merge_tx.clone();
                                merge_jobs.spawn_blocking(move || {
                                    let prepared = merge_view::prepare(
                                        &paths,
                                        &project,
                                        &record.task,
                                        &record.paths,
                                    );
                                    let _ = events.send(MergeEvent::Loaded(prepared));
                                });
                            }
                            Effect::Merge(action) => {
                                let Some(state) = app.active_merge.as_ref() else {
                                    continue;
                                };
                                let Some(plan) = state.plan.clone() else {
                                    continue;
                                };
                                let task = state.task.clone();
                                let paths = state.paths.clone();
                                let events = merge_tx.clone();
                                merge_jobs.spawn_blocking(move || {
                                    let update = merge_view::execute(plan, task, &paths, action);
                                    let _ = events.send(MergeEvent::Updated(update));
                                });
                            }
                            Effect::CheckUpdates => {
                                let events = system_tx.clone();
                                system_jobs.spawn_blocking(move || {
                                    let status =
                                        crate::update::check_latest(env!("CARGO_PKG_VERSION"));
                                    let _ = events.send(SystemEvent::Update(status));
                                });
                            }
                            Effect::OpenRepository => {
                                let events = system_tx.clone();
                                system_jobs.spawn_blocking(move || {
                                    let opened = crate::open::open_url(crate::REPO_URL);
                                    let _ = events.send(SystemEvent::RepositoryOpened(opened));
                                });
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if app.should_quit {
            break;
        }

        tokio::select! {
            _ = ticker.tick() => app.on_tick(),
            Some(event) = analysis_rx.recv() => app.on_analysis_event(event),
            Some(event) = task_rx.recv() => app.on_task_event(event),
            Some(event) = merge_rx.recv() => app.on_merge_event(event),
            Some(event) = system_rx.recv() => app.on_system_event(event),
        }
    }
    analysis_jobs.abort_all();
    while analysis_jobs.join_next().await.is_some() {}
    merge_jobs.abort_all();
    while merge_jobs.join_next().await.is_some() {}
    system_jobs.abort_all();
    while system_jobs.join_next().await.is_some() {}
    if let Some(commands) = &app.task_commands {
        let _ = commands.send(task::runtime::TaskRunCommand::Abort);
    }
    if let Some(mut job) = task_job {
        if tokio::time::timeout(Duration::from_secs(25), &mut job)
            .await
            .is_err()
        {
            job.abort();
            let _ = job.await;
        }
    }
    Ok(())
}

/// Deliberately panic mid-render, to prove the terminal is restored (M1 acceptance criterion).
///
/// Verified for real rather than trusted: the panic happens *inside* `terminal.draw`, with raw
/// mode on and the alternate screen active — the exact situation that leaves a wrecked terminal
/// if the hook is missing.
pub fn panic_test() -> io::Result<()> {
    let mut terminal = ratatui::init();
    let result = terminal.draw(|frame| {
        use ratatui::widgets::Paragraph;
        frame.render_widget(Paragraph::new("about to panic..."), frame.area());
        panic!("reporch --panic-test: deliberate panic inside terminal.draw");
    });
    ratatui::restore();
    result.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::at(d.path().join("home"));
        std::fs::create_dir_all(p.projects_dir()).unwrap();
        // Leak the tempdir for the life of the test: we only need the paths to exist.
        std::mem::forget(d);
        App::new(p, Config::default())
    }

    fn sample_task(status: reporch_core::TaskStatus) -> reporch_core::Task {
        reporch_core::Task {
            schema_version: 1,
            id: "TASK-0001".parse().unwrap(),
            name: "scoped-api-keys".into(),
            status,
            created_at: "2026-08-12T00:00:00Z".into(),
            started_at: Some("2026-08-12T00:00:01Z".into()),
            finished_at: (!status.is_running()).then_some("2026-08-12T00:01:00Z".into()),
            master_repo: "modulex".into(),
            model: "sonnet".into(),
            master_effort: reporch_core::Effort::Medium,
            worker_effort: reporch_core::Effort::Low,
            ultracode: false,
            budget_usd: 1.0,
            spent_usd: 0.0,
            branch: "reporch/TASK-0001-scoped-api-keys".into(),
            repositories: Vec::new(),
        }
    }

    fn press(app: &mut App, code: KeyCode) {
        let _ = app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn q_quits_from_the_main_menu() {
        let mut a = app();
        press(&mut a, KeyCode::Char('q'));
        assert!(a.should_quit);
    }

    #[test]
    fn ctrl_c_quits_from_anywhere() {
        for screen in [
            Screen::Main,
            Screen::Help,
            Screen::Notice {
                title: "t".into(),
                body: "b".into(),
            },
        ] {
            let mut a = app();
            a.screen = screen.clone();
            let _ = a.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
            assert!(a.should_quit, "Ctrl-C must exit from {screen:?}");
        }
    }

    #[test]
    fn arrows_and_vim_keys_both_navigate() {
        let mut a = app();
        assert_eq!(a.main_menu.selected(), MenuItem::NewProject);
        press(&mut a, KeyCode::Down);
        assert_eq!(a.main_menu.selected(), MenuItem::ListProjects);
        press(&mut a, KeyCode::Char('j'));
        assert_eq!(a.main_menu.selected(), MenuItem::Help);
        press(&mut a, KeyCode::Up);
        assert_eq!(a.main_menu.selected(), MenuItem::ListProjects);
        press(&mut a, KeyCode::Char('k'));
        assert_eq!(a.main_menu.selected(), MenuItem::NewProject);
    }

    #[test]
    fn selection_wraps_in_both_directions() {
        let mut a = app();
        // Up from the first item lands on the last.
        press(&mut a, KeyCode::Up);
        assert_eq!(a.main_menu.selected(), MenuItem::Star);
        press(&mut a, KeyCode::Down);
        assert_eq!(a.main_menu.selected(), MenuItem::NewProject);
    }

    #[test]
    fn help_opens_and_escapes_back() {
        let mut a = app();
        press(&mut a, KeyCode::Char('j'));
        press(&mut a, KeyCode::Char('j'));
        assert_eq!(a.main_menu.selected(), MenuItem::Help);
        press(&mut a, KeyCode::Enter);
        assert_eq!(a.screen, Screen::Help);
        press(&mut a, KeyCode::Esc);
        assert_eq!(a.screen, Screen::Main);
        assert!(!a.should_quit, "escaping a screen is not quitting");
    }

    #[test]
    fn update_and_star_actions_leave_external_work_to_background_effects() {
        let mut a = app();
        a.main_menu.select(3);
        let update = a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(update, Some(Effect::CheckUpdates)));
        assert!(matches!(
            a.screen,
            Screen::Notice {
                ref title,
                ref body
            } if title == "check updates" && body.contains("Checking")
        ));

        a.screen = Screen::Main;
        a.main_menu.select(4);
        let star = a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(star, Some(Effect::OpenRepository)));
        assert_eq!(a.screen, Screen::Main);
        assert!(a
            .status
            .as_deref()
            .is_some_and(|status| status.contains(crate::REPO_URL)));
    }

    #[test]
    fn q_inside_a_subscreen_goes_back_rather_than_quitting() {
        let mut a = app();
        a.screen = Screen::Help;
        press(&mut a, KeyCode::Char('q'));
        assert_eq!(a.screen, Screen::Main);
        assert!(!a.should_quit);
    }

    #[test]
    fn list_projects_is_honest_when_there_are_none() {
        let mut a = app();
        press(&mut a, KeyCode::Down);
        press(&mut a, KeyCode::Enter);
        match &a.screen {
            Screen::Notice { body, .. } => assert!(body.contains("No projects yet")),
            other => panic!("expected a notice, got {other:?}"),
        }
    }

    #[test]
    fn selecting_a_project_opens_its_menu_before_any_task_flow() {
        let mut a = app();
        let project = reporch_core::ProjectBase {
            schema_version: 1,
            name: "modulex".into(),
            created_at: "2026-08-12T00:00:00Z".into(),
            analyzed_at: Some("2026-08-12T00:00:01Z".into()),
            overview: "A multi-repo product.".into(),
            repositories: Vec::new(),
            graph: std::collections::BTreeMap::new(),
            settings: Default::default(),
            next_task_id: 1,
        };
        std::fs::create_dir_all(a.paths.project_dir("modulex")).unwrap();
        reporch_store::atomic::write_json(&a.paths.project_base_json("modulex"), &project).unwrap();
        a.projects = vec!["modulex".into()];
        a.screen = Screen::Projects;

        press(&mut a, KeyCode::Enter);

        assert_eq!(a.screen, Screen::Project);
        assert_eq!(
            a.project.as_ref().map(|project| project.name.as_str()),
            Some("modulex")
        );
        assert!(a.wizard.is_none());
    }

    #[test]
    fn project_settings_persist_budget_carry_over_and_confirmed_repo_drop() {
        let mut a = app();
        let repo = |name: &str| reporch_core::ProjectRepo {
            name: name.into(),
            path: format!("/repos/{name}"),
            remote_url: None,
            default_branch: Some("main".into()),
            purpose: String::new(),
            profile_ref: None,
            cross_session_inbound: None,
        };
        let project = reporch_core::ProjectBase {
            schema_version: 1,
            name: "modulex".into(),
            created_at: "2026-08-12T00:00:00Z".into(),
            analyzed_at: Some("2026-08-12T00:00:01Z".into()),
            overview: String::new(),
            repositories: vec![repo("api"), repo("ui")],
            graph: std::collections::BTreeMap::new(),
            settings: Default::default(),
            next_task_id: 1,
        };
        std::fs::create_dir_all(a.paths.project_dir("modulex")).unwrap();
        reporch_store::atomic::write_json(&a.paths.project_base_json("modulex"), &project).unwrap();
        a.project = Some(project);
        a.screen = Screen::Settings;

        a.settings.cursor = 1;
        press(&mut a, KeyCode::Right);
        assert_eq!(
            a.project
                .as_ref()
                .unwrap()
                .settings
                .max_budget_usd_per_session,
            11.0
        );

        a.settings.cursor = 2;
        press(&mut a, KeyCode::Enter);
        for ch in ".env.local".chars() {
            press(&mut a, KeyCode::Char(ch));
        }
        press(&mut a, KeyCode::Enter);
        assert_eq!(
            a.project.as_ref().unwrap().settings.worktree_carry_over[0].pattern,
            ".env.local"
        );

        a.settings.cursor = 4;
        press(&mut a, KeyCode::Char('d'));
        assert_eq!(a.settings.confirm_drop.as_deref(), Some("api"));
        press(&mut a, KeyCode::Char('y'));
        assert_eq!(a.project.as_ref().unwrap().repo_names(), vec!["ui"]);

        let persisted: reporch_core::ProjectBase =
            reporch_store::atomic::read_json(&a.paths.project_base_json("modulex")).unwrap();
        assert_eq!(persisted.settings.max_budget_usd_per_session, 11.0);
        assert_eq!(persisted.settings.worktree_carry_over.len(), 1);
        assert_eq!(persisted.repo_names(), vec!["ui"]);
    }

    #[test]
    fn completed_task_requires_a_literal_y_key_before_the_merge_actor_can_run() {
        let mut a = app();
        let task = reporch_core::Task {
            schema_version: 1,
            id: "TASK-0001".parse().unwrap(),
            name: "real-output".into(),
            status: reporch_core::TaskStatus::Completed,
            created_at: "2026-08-12T00:00:00Z".into(),
            started_at: None,
            finished_at: Some("2026-08-12T00:01:00Z".into()),
            master_repo: "api".into(),
            model: "sonnet".into(),
            master_effort: reporch_core::Effort::Medium,
            worker_effort: reporch_core::Effort::Low,
            ultracode: false,
            budget_usd: 1.0,
            spent_usd: 0.2,
            branch: "reporch/TASK-0001-real-output".into(),
            repositories: vec![reporch_core::TaskRepo {
                name: "api".into(),
                path: "/missing/source".into(),
                role: reporch_core::Role::Master,
                worktree_path: "/missing/worktree".into(),
                branch: "reporch/TASK-0001-real-output".into(),
                base_commit: "0".repeat(40),
                session_alias: "api@TASK-0001".into(),
                session_id: "session".into(),
                session_pid: None,
                model: "sonnet".into(),
                effort: reporch_core::Effort::Medium,
                ultracode: false,
                state: reporch_core::RepoState::Done,
                changed: Some(true),
                last_activity_at: None,
                spent_usd: 0.2,
                token_usage: Default::default(),
            }],
        };
        let task_paths = reporch_store::TaskPaths::at(a.paths.root().join("task"));
        let record = crate::project::catalog::TaskRecord {
            task: task.clone(),
            paths: task_paths.clone(),
        };
        let project = reporch_core::ProjectBase {
            schema_version: 1,
            name: "project".into(),
            created_at: "2026-08-12T00:00:00Z".into(),
            analyzed_at: None,
            overview: String::new(),
            repositories: Vec::new(),
            graph: std::collections::BTreeMap::new(),
            settings: Default::default(),
            next_task_id: 2,
        };
        a.project = Some(project.clone());
        a.task_catalog.tasks.push(record);
        a.screen = Screen::TaskList(project_views::TaskListKind::Completed);

        let load = a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(matches!(load, Some(Effect::LoadMerge { .. })));
        assert_eq!(a.screen, Screen::Merge);
        assert!(a.active_merge.as_ref().unwrap().loading);

        let report = task::ResultReport {
            summary: "Implemented real output.".into(),
            repositories: vec![task::merge::ResultRepo {
                name: "api".into(),
                changed: true,
                files: Some(1),
                tests: Some("passing".into()),
                summary: Some("Feature".into()),
                merge_notes: Some(String::new()),
                reason: None,
            }],
            merge_order: vec!["api".into()],
            risks: Vec::new(),
        };
        let graph =
            reporch_core::DepGraph::build(&["api".into()], &std::collections::BTreeMap::new());
        let plan = task::MergePlan::build(&task, &report, &graph);
        a.on_merge_event(MergeEvent::Loaded(Ok(merge_view::PreparedMerge {
            report: task::LoadedReport {
                report,
                warnings: Vec::new(),
            },
            plan,
            current_diff: "feature.txt | 1 +".into(),
        })));

        assert!(a
            .on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .is_none());
        assert!(!a.active_merge.as_ref().unwrap().busy);
        assert!(matches!(
            a.on_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)),
            Some(Effect::Merge(merge_view::MergeAction::Confirm(_)))
        ));
        assert!(a.active_merge.as_ref().unwrap().busy);
    }

    #[test]
    fn consenting_stays_in_the_flow_and_starts_live_analysis() {
        let mut a = app();
        let mut wizard = Wizard::new();
        wizard.project_name = "modulex".into();
        wizard.step = wizard::Step::Consent;
        wizard.selected.insert("modulex".into());
        wizard.candidates.push(reporch_scan::FoundRepo {
            name: "modulex".into(),
            path: a.paths.root().join("repos/modulex"),
            remote_url: None,
            remote_org: None,
            head_age_secs: None,
        });
        a.wizard = Some(wizard);
        a.screen = Screen::Flow;

        let effect = a.on_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));

        assert_eq!(a.screen, Screen::Flow, "analysis belongs in the console");
        assert_eq!(
            a.wizard.as_ref().map(|w| &w.step),
            Some(&wizard::Step::Analysing)
        );
        assert!(matches!(effect, Some(Effect::StartAnalysis(_))));
    }

    #[test]
    fn analysis_events_drive_the_visible_progress_and_finish_in_place() {
        let mut a = app();
        let mut wizard = Wizard::new();
        wizard.project_name = "modulex".into();
        wizard.step = wizard::Step::Analysing;
        wizard.analysis = vec![("modulex".into(), wizard::AnalysisState::Waiting)];
        a.wizard = Some(wizard);
        a.screen = Screen::Flow;

        a.on_analysis_event(flow::AnalysisEvent::Running {
            repo: "modulex".into(),
        });
        assert!(matches!(
            a.wizard.as_ref().unwrap().analysis[0].1,
            wizard::AnalysisState::Running { .. }
        ));
        a.on_analysis_event(flow::AnalysisEvent::Done {
            repo: "modulex".into(),
            one_liner: "Owns the API contract.".into(),
        });
        assert_eq!(
            a.wizard.as_ref().unwrap().analysis[0].1,
            wizard::AnalysisState::Done {
                one_liner: "Owns the API contract.".into()
            }
        );

        let project = reporch_core::ProjectBase {
            schema_version: 1,
            name: "modulex".into(),
            created_at: "2026-08-12T00:00:00Z".into(),
            analyzed_at: Some("2026-08-12T00:00:01Z".into()),
            overview: "A multi-repo product.".into(),
            repositories: Vec::new(),
            graph: std::collections::BTreeMap::new(),
            settings: Default::default(),
            next_task_id: 1,
        };
        a.on_analysis_event(flow::AnalysisEvent::Finished(Box::new(Ok(project))));

        assert_eq!(a.screen, Screen::Flow);
        assert_eq!(a.project.as_ref().map(|p| p.name.as_str()), Some("modulex"));
        assert_eq!(
            a.wizard.as_ref().map(|w| &w.step),
            Some(&wizard::Step::Done)
        );
    }

    #[test]
    fn starting_a_task_produces_a_runtime_effect_instead_of_a_handoff() {
        let mut a = app();
        let project = reporch_core::ProjectBase {
            schema_version: 1,
            name: "modulex".into(),
            created_at: "2026-08-12T00:00:00Z".into(),
            analyzed_at: Some("2026-08-12T00:00:01Z".into()),
            overview: String::new(),
            repositories: vec![reporch_core::ProjectRepo {
                name: "modulex".into(),
                path: a.paths.root().join("repos/modulex").display().to_string(),
                remote_url: None,
                default_branch: Some("main".into()),
                purpose: "Owns the API.".into(),
                profile_ref: None,
                cross_session_inbound: None,
            }],
            graph: std::collections::BTreeMap::new(),
            settings: reporch_core::ProjectSettings {
                consented_at: Some("2026-08-12T00:00:00Z".into()),
                ..Default::default()
            },
            next_task_id: 1,
        };
        let mut wizard = Wizard::for_task(&project);
        wizard.step = wizard::Step::ConfirmMaster;
        wizard.brief = "Add scoped API keys.".into();
        wizard.master = "modulex".into();
        a.project = Some(project);
        a.wizard = Some(wizard);
        a.screen = Screen::Flow;

        let effect = a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(
            a.screen,
            Screen::Flow,
            "startup progress belongs in the console"
        );
        assert_eq!(
            a.wizard.as_ref().map(|w| &w.step),
            Some(&wizard::Step::Running)
        );
        assert!(matches!(effect, Some(Effect::StartTask(_))));
    }

    #[test]
    fn every_advertised_run_key_opens_a_view_or_sends_a_confirmed_command() {
        let mut a = app();
        let task = reporch_core::Task {
            schema_version: 1,
            id: "TASK-0001".parse().unwrap(),
            name: "scoped-api-keys".into(),
            status: reporch_core::TaskStatus::Active,
            created_at: "2026-08-12T00:00:00Z".into(),
            started_at: Some("2026-08-12T00:00:01Z".into()),
            finished_at: None,
            master_repo: "modulex".into(),
            model: "sonnet".into(),
            master_effort: reporch_core::Effort::Medium,
            worker_effort: reporch_core::Effort::Low,
            ultracode: false,
            budget_usd: 1.0,
            spent_usd: 0.0,
            branch: "reporch/TASK-0001-scoped-api-keys".into(),
            repositories: vec![reporch_core::TaskRepo {
                name: "modulex".into(),
                path: "/repos/modulex".into(),
                role: reporch_core::Role::Master,
                worktree_path: "/worktrees/modulex".into(),
                branch: "reporch/TASK-0001-scoped-api-keys".into(),
                base_commit: "abc123".into(),
                session_alias: "modulex@TASK-0001".into(),
                session_id: "session-1".into(),
                session_pid: Some(1234),
                model: "sonnet".into(),
                effort: reporch_core::Effort::Medium,
                ultracode: false,
                state: reporch_core::RepoState::Working,
                changed: None,
                last_activity_at: None,
                spent_usd: 0.0,
                token_usage: Default::default(),
            }],
        };
        a.on_task_event(task::runtime::TaskRunEvent::Started(Box::new(
            task::runtime::StartedTask {
                task,
                paths: reporch_store::TaskPaths::at(a.paths.root().join("tasks/t")),
                warnings: Vec::new(),
            },
        )));
        a.on_task_event(task::runtime::TaskRunEvent::View(task::RunView {
            rows: vec![task::RunRow {
                repo: "modulex".into(),
                role: reporch_core::Role::Master,
                state: reporch_core::RepoState::Working,
                last: "editing src/lib.rs".into(),
                briefs_in: 0,
                briefs_out: 0,
                spent_usd: 0.2,
                token_usage: Default::default(),
            }],
            budget_usd: 1.0,
            ..Default::default()
        }));

        assert_eq!(a.screen, Screen::Run);
        assert_eq!(a.active_run.as_ref().unwrap().view.budget_usd, 1.0);

        assert!(a
            .on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .is_none());
        assert_eq!(
            a.screen,
            Screen::SessionLog {
                repo: "modulex".into()
            }
        );
        press(&mut a, KeyCode::Esc);
        press(&mut a, KeyCode::Char('b'));
        assert_eq!(a.screen, Screen::Mailbox);
        press(&mut a, KeyCode::Esc);
        press(&mut a, KeyCode::Char('l'));
        assert_eq!(a.screen, Screen::TaskLogs);
        press(&mut a, KeyCode::Esc);

        assert!(matches!(
            a.on_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE)),
            Some(Effect::SetTaskPaused(true))
        ));
        press(&mut a, KeyCode::Char('x'));
        assert!(a.active_run.as_ref().unwrap().confirm_abort);
        assert!(matches!(
            a.on_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)),
            Some(Effect::AbortTask)
        ));
    }

    #[test]
    fn a_stopped_completed_run_returns_to_the_project_menu() {
        let mut a = app();
        a.screen = Screen::Run;
        a.active_run = Some(ActiveRun {
            task: sample_task(reporch_core::TaskStatus::Completed),
            paths: reporch_store::TaskPaths::at(a.paths.root().join("tasks/t")),
            started_at: Instant::now(),
            view: task::RunView {
                finished: Some(task::FinishReason::Completed),
                ..Default::default()
            },
            confirm_abort: false,
        });
        a.on_task_event(task::runtime::TaskRunEvent::Stopped);

        press(&mut a, KeyCode::Esc);

        assert_eq!(a.screen, Screen::Project);
        assert!(a.active_run.is_none());
    }

    #[test]
    fn unknown_keys_do_nothing() {
        let mut a = app();
        let before = a.main_menu.selected();
        press(&mut a, KeyCode::Char('z'));
        press(&mut a, KeyCode::F(5));
        assert_eq!(a.main_menu.selected(), before);
        assert!(!a.should_quit);
    }
}
