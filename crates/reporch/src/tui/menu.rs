//! The top-level menu (§5.1) and its rendering.
//!
//! ```text
//!   reporch — one task, every repo, one console
//!
//!   › new project
//!     list projects
//!     help
//!     check updates
//!     ★ star reporch on github
//! ```

use ratatui::layout::{Constraint, Layout};
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Padding, Paragraph, Wrap};
use ratatui::Frame;

use super::{App, Screen};
use crate::tui::theme;

/// The five entries of §5.1, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuItem {
    NewProject,
    ListProjects,
    Help,
    CheckUpdates,
    Star,
}

impl MenuItem {
    pub const ALL: [MenuItem; 5] = [
        MenuItem::NewProject,
        MenuItem::ListProjects,
        MenuItem::Help,
        MenuItem::CheckUpdates,
        MenuItem::Star,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            MenuItem::NewProject => "new project",
            MenuItem::ListProjects => "list projects",
            MenuItem::Help => "help",
            MenuItem::CheckUpdates => "check updates",
            MenuItem::Star => "★ star reporch on github",
        }
    }
}

/// Selection state. It lives here, in the app, not in the widget — that is the immediate-mode
/// contract (`docs/ratatui-notes.md` §4).
#[derive(Debug)]
pub struct MainMenu {
    pub state: ListState,
}

impl Default for MainMenu {
    fn default() -> Self {
        Self::new()
    }
}

impl MainMenu {
    pub fn new() -> Self {
        let mut state = ListState::default();
        state.select(Some(0));
        Self { state }
    }

    pub fn index(&self) -> usize {
        self.state.selected().unwrap_or(0)
    }

    pub fn selected(&self) -> MenuItem {
        MenuItem::ALL[self.index().min(MenuItem::ALL.len() - 1)]
    }

    pub fn select(&mut self, i: usize) {
        self.state.select(Some(i.min(MenuItem::ALL.len() - 1)));
    }

    /// Wrapping, deliberately. `ListState::select_next` saturates, which is right for a long
    /// scrolling list and wrong for a five-item menu.
    pub fn next(&mut self) {
        let i = (self.index() + 1) % MenuItem::ALL.len();
        self.state.select(Some(i));
    }

    pub fn previous(&mut self) {
        let n = MenuItem::ALL.len();
        let i = (self.index() + n - 1) % n;
        self.state.select(Some(i));
    }
}

/// Draw the whole frame. A pure projection of `App` — nothing is retained between frames.
pub fn draw(frame: &mut Frame, app: &mut App) {
    if app.screen == Screen::Merge {
        if let Some(state) = &app.active_merge {
            crate::tui::merge_view::draw(frame, frame.area(), state);
        }
        return;
    }
    if app.screen == Screen::Run {
        if let Some(active) = &app.active_run {
            crate::tui::run::draw(
                frame,
                frame.area(),
                crate::tui::run::RunMeta {
                    task_id: &active.task.id.to_string(),
                    slug: &active.task.name,
                    elapsed_secs: active.started_at.elapsed().as_secs(),
                    confirm_abort: active.confirm_abort,
                },
                &active.view,
                &mut app.run_table,
            );
        }
        return;
    }
    if matches!(
        app.screen,
        Screen::SessionLog { .. } | Screen::Mailbox | Screen::TaskLogs
    ) {
        let title = match &app.screen {
            Screen::SessionLog { repo } => format!("session output · {repo}"),
            Screen::Mailbox => "task mailbox · briefs are untrusted data".into(),
            Screen::TaskLogs => "all session logs · grouped by repository".into(),
            _ => unreachable!(),
        };
        let entries = app.task_diagnostic_entries();
        crate::tui::task_views::draw(
            frame,
            frame.area(),
            &title,
            &entries,
            app.task_view_cursor,
            app.task_view_detail_scroll,
        );
        return;
    }
    match app.screen.clone() {
        Screen::Project => {
            if let Some(project) = &app.project {
                crate::tui::project_views::draw_project(
                    frame,
                    frame.area(),
                    project,
                    &app.task_catalog,
                    app.project_menu_cursor,
                );
            }
            return;
        }
        Screen::TaskList(kind) => {
            if let Some(project) = &app.project {
                crate::tui::project_views::draw_tasks(
                    frame,
                    frame.area(),
                    project,
                    &app.task_catalog,
                    kind,
                    app.task_cursor,
                );
            }
            return;
        }
        Screen::TaskDetail => {
            if let Some(record) = &app.selected_task {
                crate::tui::project_views::draw_task(frame, frame.area(), record);
            }
            return;
        }
        Screen::Settings => {
            if let Some(project) = &app.project {
                crate::tui::project_views::draw_settings(
                    frame,
                    frame.area(),
                    project,
                    &app.settings,
                );
            }
            return;
        }
        _ => {}
    }

    let [header, body, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("  "),
            "reporch".bold().fg(theme::ACCENT),
            Span::raw(" — one task, every repo, one console").fg(theme::DIM),
        ])),
        header,
    );

    match app.screen.clone() {
        Screen::Main => draw_menu(frame, body, app),
        Screen::Help => draw_text(frame, body, "help", HELP),
        Screen::Notice { title, body: text } => draw_text(frame, body, &title, &text),
        Screen::Projects => draw_projects(frame, body, app),
        Screen::Project
        | Screen::TaskList(_)
        | Screen::TaskDetail
        | Screen::Settings
        | Screen::Merge => {
            unreachable!("project views are drawn before the menu layout")
        }
        Screen::Run => unreachable!("the run screen is drawn before the menu layout"),
        Screen::SessionLog { .. } | Screen::Mailbox | Screen::TaskLogs => {
            unreachable!("task views are drawn before the menu layout")
        }
        Screen::Flow => {
            if let Some(w) = &app.wizard {
                // The flow draws its own footer, so it gets the body and the footer row.
                let area = body.union(footer);
                let mut state = std::mem::take(&mut app.list_state);
                crate::tui::flow::draw(
                    frame,
                    area,
                    w,
                    &mut state,
                    app.config.session.max_budget_usd_per_session,
                );
                app.list_state = state;
                return;
            }
            draw_menu(frame, body, app)
        }
    }

    let hint = match app.screen {
        Screen::Main => "↑↓ move · enter select · q quit",
        Screen::Projects => "↑↓ move · enter new task · esc back",
        Screen::Project
        | Screen::TaskList(_)
        | Screen::TaskDetail
        | Screen::Settings
        | Screen::Merge => "esc project",
        Screen::Run => "↑↓ move · x abort",
        Screen::SessionLog { .. } | Screen::Mailbox | Screen::TaskLogs => "esc run",
        _ => "esc back · q back · ctrl-c quit",
    };
    let line = match &app.status {
        Some(s) => Line::from(vec![
            Span::raw("  "),
            Span::raw(s.clone()).fg(theme::ACCENT),
        ]),
        None => Line::from(Span::raw(format!("  {hint}")).fg(theme::DIM)),
    };
    frame.render_widget(Paragraph::new(line), footer);
}

fn draw_menu(frame: &mut Frame, area: ratatui::layout::Rect, app: &mut App) {
    let items: Vec<ListItem> = MenuItem::ALL
        .iter()
        .map(|it| ListItem::new(Line::from(Span::raw(it.label()))))
        .collect();

    let list = List::new(items)
        .block(Block::default().padding(Padding::new(2, 2, 1, 0)))
        .highlight_symbol("› ")
        .highlight_style(theme::selected())
        // Keeps unselected rows aligned with selected ones instead of shifting by two columns.
        .highlight_spacing(ratatui::widgets::HighlightSpacing::Always);

    frame.render_stateful_widget(list, area, &mut app.main_menu.state);
}

/// The project list (§5.1 `list projects`).
fn draw_projects(frame: &mut Frame, area: ratatui::layout::Rect, app: &mut App) {
    let items: Vec<ListItem> = app
        .projects
        .iter()
        .map(|p| ListItem::new(Line::from(Span::raw(p.clone()))))
        .collect();
    let mut state = ListState::default();
    state.select(Some(app.projects_cursor.min(items.len().saturating_sub(1))));
    frame.render_stateful_widget(
        List::new(items)
            .block(
                Block::default()
                    .title(Line::from(Span::raw(" projects ").fg(theme::ACCENT)))
                    .padding(Padding::new(2, 2, 1, 0)),
            )
            .highlight_style(theme::selected())
            .highlight_symbol("› ")
            .highlight_spacing(ratatui::widgets::HighlightSpacing::Always),
        area,
        &mut state,
    );
}

fn draw_text(frame: &mut Frame, area: ratatui::layout::Rect, title: &str, body: &str) {
    let block = Block::default()
        .title(Line::from(
            Span::raw(format!(" {title} ")).fg(theme::ACCENT),
        ))
        .padding(Padding::new(2, 2, 1, 0));
    frame.render_widget(
        Paragraph::new(body).block(block).wrap(Wrap { trim: false }),
        area,
    );
}

const HELP: &str = "\
reporch runs one development task across every repository of a multi-repo project, by
orchestrating a Claude Code session inside each one.

  new project     Name a project, pick its repositories, and let reporch read them.
  list projects   Open a project to start a task, watch one run, or merge a finished one.
  check updates   See whether a newer reporch exists.

The idea it is built on, in one line:

  Briefs are content. Pings are attention.

Anyone may write a brief to anyone. Only the master repo may ping a worker; workers ping
nobody. When a worker needs another worker to act, it writes the brief directly and asks
the master to deliver the ping. reporch itself pings on a worker's behalf when a task
would otherwise stall — rate-limited, and logged with the reason.

Those rules are enforced in the binary, not in a prompt: spawned sessions have the
messaging tools removed outright, and all traffic goes through the `reporch` CLI, which
knows the caller's role.

Everything lives in ~/.reporch, in plain JSON and markdown. Nothing is hidden from you.

  reporch doctor        Check that this machine can run reporch.
  reporch protocol      Print the orchestration rules in full.

Merging is always confirmed by you, per repository.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_menu_matches_section_5_1_exactly() {
        let labels: Vec<&str> = MenuItem::ALL.iter().map(|m| m.label()).collect();
        assert_eq!(
            labels,
            vec![
                "new project",
                "list projects",
                "help",
                "check updates",
                "★ star reporch on github",
            ]
        );
    }

    #[test]
    fn selection_starts_on_the_first_item() {
        let m = MainMenu::new();
        assert_eq!(m.index(), 0);
        assert_eq!(m.selected(), MenuItem::NewProject);
    }

    #[test]
    fn navigation_wraps_around() {
        let mut m = MainMenu::new();
        for _ in 0..MenuItem::ALL.len() {
            m.next();
        }
        assert_eq!(m.index(), 0, "a full cycle returns to the start");
        m.previous();
        assert_eq!(m.selected(), MenuItem::Star);
    }

    #[test]
    fn select_clamps_rather_than_panicking() {
        let mut m = MainMenu::new();
        m.select(999);
        assert_eq!(m.selected(), MenuItem::Star);
    }

    /// A corrupt or out-of-range index must not panic when read — the menu is drawn every frame.
    #[test]
    fn selected_is_total() {
        let mut m = MainMenu::new();
        m.state.select(Some(usize::MAX));
        assert_eq!(m.selected(), MenuItem::Star);
        m.state.select(None);
        assert_eq!(m.selected(), MenuItem::NewProject);
    }

    /// The rendered frame is asserted against a real buffer, so a layout regression is caught
    /// without a person having to look at it.
    #[test]
    fn the_menu_renders_the_selection_marker() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let d = tempfile::tempdir().unwrap();
        let mut app = App::new(
            reporch_store::Paths::at(d.path()),
            reporch_store::Config::default(),
        );
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();

        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("reporch"), "{rendered}");
        assert!(rendered.contains("› new project"), "{rendered}");
        assert!(rendered.contains("list projects"), "{rendered}");
        assert!(rendered.contains("★ star reporch on github"), "{rendered}");
        assert!(rendered.contains("q quit"), "{rendered}");
    }

    #[test]
    fn the_marker_moves_with_the_selection() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let d = tempfile::tempdir().unwrap();
        let mut app = App::new(
            reporch_store::Paths::at(d.path()),
            reporch_store::Config::default(),
        );
        app.main_menu.next();
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("› list projects"), "{rendered}");
    }

    #[test]
    fn project_consent_renders_the_configured_session_budget() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let d = tempfile::tempdir().unwrap();
        let mut config = reporch_store::Config::default();
        config.session.max_budget_usd_per_session = 0.25;
        let mut app = App::new(reporch_store::Paths::at(d.path()), config);
        let mut wizard = crate::tui::wizard::Wizard::new();
        wizard.project_name = "contracts".into();
        wizard.selected = ["api", "client", "docs"]
            .into_iter()
            .map(str::to_string)
            .collect();
        wizard.step = crate::tui::wizard::Step::Consent;
        app.wizard = Some(wizard);
        app.screen = Screen::Flow;

        let mut terminal = Terminal::new(TestBackend::new(96, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let rendered = terminal.backend().to_string();
        assert!(
            rendered.contains("hard spend ceiling of $0.25 per session"),
            "the consent must describe the setting that will be persisted: {rendered}"
        );
    }

    #[test]
    fn help_text_states_the_central_rule() {
        assert!(HELP.contains("Briefs are content. Pings are attention."));
        assert!(HELP.contains("workers ping"));
        assert!(HELP.contains("Merging is always confirmed by you"));
    }
}
