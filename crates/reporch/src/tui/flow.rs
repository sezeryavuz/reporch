//! Driving the wizard: key handling, rendering, and the work each step performs.
//!
//! Kept apart from [`crate::tui::wizard`], which is pure state, so the flow logic stays testable
//! without a terminal and the rendering stays a projection of it.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Padding, Paragraph, Wrap};
use ratatui::Frame;

use reporch_core::{Effort, ProjectBase};
use reporch_scan::FoundRepo;
use reporch_store::{Config, Paths};

use super::theme;
use super::wizard::{AnalysisState, Step, Wizard, MODELS};

/// What the app should do after a key press. The wizard never performs I/O itself.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    None,
    /// Leave the flow.
    Cancel,
    /// Scan the machine for repositories (§5.3).
    Scan,
    /// The user consented; write it and start analysis (§12.1, §5.4).
    ConsentGiven,
    /// Analyse the selected repositories.
    Analyse,
    /// Create the task and start it (§5.6–§5.8).
    StartTask,
}

/// Immutable input handed from the immediate-mode UI to the background analysis task. Keeping
/// the wizard out of the task means rendering and key handling never wait on Claude.
#[derive(Debug, Clone)]
pub struct AnalysisRequest {
    pub project_name: String,
    pub selected: Vec<FoundRepo>,
    pub paths: Paths,
    pub config: Config,
    /// Existing project metadata to preserve during a settings re-analysis.
    pub preserve: Option<ProjectBase>,
}

/// One observable per-repository transition from the background analysis (§5.4).
#[derive(Debug, Clone, PartialEq)]
pub enum AnalysisEvent {
    Running { repo: String },
    Done { repo: String, one_liner: String },
    Failed { repo: String, reason: String },
    Finished(Box<Result<ProjectBase, String>>),
}

/// Own the complete background job, including a terminal event on every success or failure path.
pub async fn run_analysis(
    request: AnalysisRequest,
    events: tokio::sync::mpsc::UnboundedSender<AnalysisEvent>,
) {
    let result = do_analyse(request, events.clone()).await;
    let _ = events.send(AnalysisEvent::Finished(Box::new(result)));
}

/// Handle one key press.
pub fn on_key(w: &mut Wizard, key: KeyEvent, project: Option<&ProjectBase>) -> Command {
    // Esc always backs out — except while typing a filter, where it clears the filter first.
    if key.code == KeyCode::Esc {
        if w.step == Step::PickRepos && w.filtering {
            w.filtering = false;
            w.filter.clear();
            w.rerank();
            return Command::None;
        }
        return Command::Cancel;
    }

    match w.step {
        Step::ProjectName => match key.code {
            KeyCode::Enter if w.can_advance() => {
                w.step = Step::Scanning;
                return Command::Scan;
            }
            KeyCode::Backspace => w.backspace(),
            KeyCode::Char(c) => w.push_char(c),
            _ => {}
        },

        Step::PickRepos => {
            if w.filtering {
                match key.code {
                    KeyCode::Enter => w.filtering = false,
                    KeyCode::Backspace => w.backspace(),
                    KeyCode::Char(c) => w.push_char(c),
                    KeyCode::Down => w.move_cursor(1),
                    KeyCode::Up => w.move_cursor(-1),
                    _ => {}
                }
            } else {
                match key.code {
                    KeyCode::Char(' ') => w.toggle_current(),
                    KeyCode::Char('/') => w.filtering = true,
                    KeyCode::Down | KeyCode::Char('j') => w.move_cursor(1),
                    KeyCode::Up | KeyCode::Char('k') => w.move_cursor(-1),
                    KeyCode::Char('a') => {
                        // Select everything currently visible — useful after filtering.
                        for r in w
                            .visible()
                            .iter()
                            .map(|r| r.repo.name.clone())
                            .collect::<Vec<_>>()
                        {
                            w.selected.insert(r);
                        }
                        w.rerank();
                    }
                    KeyCode::Enter if w.can_advance() => w.step = Step::Consent,
                    _ => {}
                }
            }
        }

        // §12.1: actively confirmed. `y` and nothing else — not Enter, which people press by
        // reflex, and not a default.
        Step::Consent => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                w.step = Step::Analysing;
                return Command::ConsentGiven;
            }
            KeyCode::Char('n') | KeyCode::Char('N') => return Command::Cancel,
            _ => {}
        },

        Step::TaskBrief => match key.code {
            // Enter inserts a newline; the brief is multi-line. Ctrl-D finishes it.
            KeyCode::Enter => w.brief.push('\n'),
            KeyCode::Backspace => w.backspace(),
            KeyCode::Char('d')
                if key.modifiers.contains(KeyModifiers::CONTROL) && w.can_advance() =>
            {
                w.step = Step::PickModel;
            }
            KeyCode::Char(c) => w.push_char(c),
            _ => {}
        },

        Step::PickModel => match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                w.model_cursor = (w.model_cursor + 1) % MODELS.len()
            }
            KeyCode::Up | KeyCode::Char('k') => {
                w.model_cursor = (w.model_cursor + MODELS.len() - 1) % MODELS.len()
            }
            KeyCode::Enter => w.step = Step::PickEffort,
            _ => {}
        },

        Step::PickEffort => match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                w.effort_cursor = (w.effort_cursor + 1) % Effort::ALL.len()
            }
            KeyCode::Up | KeyCode::Char('k') => {
                w.effort_cursor = (w.effort_cursor + Effort::ALL.len() - 1) % Effort::ALL.len()
            }
            KeyCode::Char('u') => w.ultracode = !w.ultracode,
            KeyCode::Enter => {
                if let Some(p) = project {
                    let (m, why) = Wizard::elect_from_graph(p);
                    w.master = m;
                    w.master_reason = why;
                }
                w.step = Step::ConfirmMaster;
            }
            _ => {}
        },

        // §5.7: shown and editable before the task starts.
        Step::ConfirmMaster => {
            let repos: Vec<String> = project.map(|p| p.repo_names()).unwrap_or_default();
            match key.code {
                KeyCode::Down | KeyCode::Char('j') => w.cycle_master(&repos, 1),
                KeyCode::Up | KeyCode::Char('k') => w.cycle_master(&repos, -1),
                KeyCode::Enter if w.can_advance() => {
                    w.step = Step::Running;
                    return Command::StartTask;
                }
                _ => {}
            }
        }

        Step::Scanning | Step::Analysing | Step::Running | Step::Done => {}
        Step::Failed(_) => return Command::Cancel,
    }
    Command::None
}

/// Perform the scan and move to the picker (§5.3).
pub fn do_scan(w: &mut Wizard, config: &Config) {
    let (found, secs) = super::wizard::scan_repos(config);
    w.candidates = found;
    w.scan_secs = secs;
    w.rerank();
    w.cursor = 0;
    w.step = Step::PickRepos;
}

/// Analyse the selected repositories in parallel (§5.4).
///
/// Failures are per-repo and non-fatal: a project with one unreadable repo is still usable, and
/// the user can re-run analysis from project settings.
pub async fn do_analyse(
    request: AnalysisRequest,
    progress: tokio::sync::mpsc::UnboundedSender<AnalysisEvent>,
) -> Result<ProjectBase, String> {
    let AnalysisRequest {
        project_name,
        selected,
        paths,
        config,
        preserve,
    } = request;
    let names: Vec<String> = selected.iter().map(|r| r.name.clone()).collect();

    let mut set = tokio::task::JoinSet::new();
    for r in &selected {
        let peers: Vec<String> = names.iter().filter(|n| **n != r.name).cloned().collect();
        let (name, path, model) = (
            r.name.clone(),
            r.path.clone(),
            config.session.default_model.clone(),
        );
        let _ = progress.send(AnalysisEvent::Running { repo: name.clone() });
        set.spawn(
            async move { crate::project::analyse_repo(&name, &path, &peers, &model, 1.0).await },
        );
    }

    let mut profiles = std::collections::BTreeMap::new();
    let mut failures = Vec::new();
    while let Some(res) = set.join_next().await {
        match res {
            Ok(crate::project::AnalysisOutcome::Ok(p)) => {
                let _ = progress.send(AnalysisEvent::Done {
                    repo: p.name.clone(),
                    one_liner: p.one_liner.clone(),
                });
                super::wizard::write_profile(&paths, &project_name, &p)
                    .map_err(|e| format!("could not write profile for `{}`: {e}", p.name))?;
                profiles.insert(p.name.clone(), *p);
            }
            Ok(crate::project::AnalysisOutcome::Failed { repo, reason }) => {
                let _ = progress.send(AnalysisEvent::Failed {
                    repo: repo.clone(),
                    reason: reason.clone(),
                });
                failures.push(format!("{repo}: {reason}"));
            }
            Err(e) => failures.push(e.to_string()),
        }
    }

    if profiles.is_empty() {
        return Err(format!(
            "no repository could be analysed.\n\n{}",
            failures.join("\n")
        ));
    }

    // The synthesis pass: one overview from all the profiles (§5.4).
    let overview = synthesise(&project_name, &profiles, &config.session.default_model)
        .await
        .unwrap_or_else(|_| String::new());

    let repos: Vec<(String, std::path::PathBuf, Option<String>)> = selected
        .iter()
        .map(|r| (r.name.clone(), r.path.clone(), r.remote_url.clone()))
        .collect();
    let mut base = crate::project::assemble(
        &project_name,
        &repos,
        &profiles,
        overview,
        reporch_store::now_rfc3339(),
    );
    if let Some(previous) = preserve {
        // Re-analysis refreshes repository facts and the dependency graph, not the user's
        // consent, task allocator, budget, hooks, or carry-over decisions (§5.5).
        base.created_at = previous.created_at;
        base.settings = previous.settings;
        base.next_task_id = previous.next_task_id;
    } else {
        // Consent was given on the §12.1 screen to reach here.
        base.settings.consented_at = Some(reporch_store::now_rfc3339());
        base.settings.default_model = config.session.default_model.clone();
        base.settings.max_budget_usd_per_session = config.session.max_budget_usd_per_session;
        // Seed the carry-over list from what analysis found (§9.2).
        let mut patterns = std::collections::BTreeSet::new();
        for p in profiles.values() {
            for u in &p.untracked_needed_for_build {
                patterns.insert(u.clone());
            }
        }
        base.settings.worktree_carry_over = patterns
            .into_iter()
            .map(|pattern| {
                let mode = match pattern.as_str() {
                    "node_modules" | ".venv" | "venv" | "target" => {
                        reporch_core::CarryMode::CopyOnWrite
                    }
                    _ => reporch_core::CarryMode::Copy,
                };
                reporch_core::CarryOver { pattern, mode }
            })
            .collect();
        for (name, p) in &profiles {
            if let Some(install) = p.commands.get("install") {
                base.settings
                    .post_create_hooks
                    .insert(name.clone(), vec![install.clone()]);
            }
        }
    }

    reporch_store::atomic::write_json(&paths.project_base_json(&project_name), &base)
        .map_err(|e| format!("could not write project_base.json: {e}"))?;
    reporch_store::atomic::write_string(
        &paths.project_base_md(&project_name),
        &crate::project::render_markdown(&base, &profiles),
    )
    .map_err(|e| format!("could not write project_base.md: {e}"))?;
    Ok(base)
}

async fn synthesise(
    project: &str,
    profiles: &std::collections::BTreeMap<String, reporch_core::RepoProfile>,
    model: &str,
) -> Result<String, String> {
    let prompt = crate::project::analyse::synthesis_prompt(project, profiles);
    let out = tokio::process::Command::new("claude")
        .args([
            "-p",
            "--model",
            model,
            "--permission-mode",
            "plan",
            "--max-budget-usd",
            "0.50",
            "--strict-mcp-config",
            &prompt,
        ])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

// ============================================================ rendering

pub fn draw(
    frame: &mut Frame,
    area: Rect,
    w: &Wizard,
    list_state: &mut ListState,
    budget_usd_per_session: f64,
) {
    let [body, footer] = Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).areas(area);

    match &w.step {
        Step::ProjectName => prompt(
            frame,
            body,
            "new project",
            "What is this project called?\n\nThe name is how reporch ranks your repositories in \
             the next step, so use the one they share.",
            &w.project_name,
        ),
        Step::Scanning => note(frame, body, "scanning", "Looking for git repositories…"),
        Step::PickRepos => pick_repos(frame, body, w, list_state),
        Step::Consent => consent(frame, body, w, budget_usd_per_session),
        Step::Analysing => analysing(frame, body, w),
        Step::TaskBrief => prompt(
            frame,
            body,
            "new task",
            "Describe the task.\n\nIt is read by the repository best placed to lead it, which \
             then delegates the rest. Say what should be true when it is done.",
            &w.brief,
        ),
        Step::PickModel => choose(
            frame,
            body,
            "model",
            "One model, used by every repository in this task — master and workers alike.",
            &MODELS
                .iter()
                .map(|(n, d)| (n.to_string(), d.to_string()))
                .collect::<Vec<_>>(),
            w.model_cursor,
            list_state,
        ),
        Step::PickEffort => effort(frame, body, w, list_state),
        Step::ConfirmMaster => master(frame, body, w),
        Step::Running => note(frame, body, "starting", "Creating worktrees and sessions…"),
        Step::Done => note(frame, body, "done", "Ready."),
        Step::Failed(e) => note(frame, body, "failed", e),
    }

    let hint = match (&w.step, w.blocked_because()) {
        (_, Some(why)) => format!("  {why}"),
        (Step::PickRepos, _) if w.filtering => "  type to filter · enter accept · esc clear".into(),
        (Step::PickRepos, _) => {
            "  space select · a select all · / filter · enter confirm · esc back".into()
        }
        (Step::Consent, _) => "  y enable · n cancel".into(),
        (Step::TaskBrief, _) => "  ctrl-d done · esc cancel".into(),
        (Step::PickEffort, _) => "  ↑↓ choose · u toggle ultracode · enter confirm".into(),
        (Step::ConfirmMaster, _) => "  ↑↓ change · enter start the task · esc back".into(),
        (Step::ProjectName, _) => "  enter continue · esc cancel".into(),
        _ => "  esc back".into(),
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::raw(hint).fg(theme::DIM))),
        footer,
    );
}

fn block(title: &str) -> Block<'_> {
    Block::default()
        .title(Line::from(
            Span::raw(format!(" {title} ")).fg(theme::ACCENT),
        ))
        .padding(Padding::new(2, 2, 1, 0))
}

fn note(frame: &mut Frame, area: Rect, title: &str, body: &str) {
    frame.render_widget(
        Paragraph::new(body)
            .block(block(title))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn prompt(frame: &mut Frame, area: Rect, title: &str, help: &str, value: &str) {
    let mut lines: Vec<Line> = help.lines().map(|l| Line::from(l.to_string())).collect();
    lines.push(Line::from(""));
    for (i, l) in value.lines().enumerate() {
        lines.push(Line::from(vec![
            Span::raw(if i == 0 { "› " } else { "  " }).fg(theme::ACCENT),
            Span::raw(l.to_string()),
        ]));
    }
    if value.is_empty() || value.ends_with('\n') {
        lines.push(Line::from(vec![
            Span::raw(if value.is_empty() { "› " } else { "  " }).fg(theme::ACCENT),
            Span::raw("█").fg(theme::DIM),
        ]));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(block(title))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn pick_repos(frame: &mut Frame, area: Rect, w: &Wizard, state: &mut ListState) {
    let visible = w.visible();
    let title = format!(
        "Select the repositories in \"{}\"                    {} selected",
        w.project_name,
        w.selected.len()
    );
    let items: Vec<ListItem> = visible
        .iter()
        .map(|r| {
            let checked = w.selected.contains(&r.repo.name);
            let org = r.repo.remote_org.clone().unwrap_or_default();
            ListItem::new(Line::from(vec![
                Span::raw(if checked { "[x] " } else { "[ ] " }).fg(if checked {
                    theme::ACCENT
                } else {
                    theme::DIM
                }),
                Span::raw(format!("{:<24}", r.repo.name)),
                Span::raw(format!(
                    "{:<38}",
                    shorten(&r.repo.path.display().to_string())
                ))
                .fg(theme::DIM),
                Span::raw(org).fg(theme::DIM),
            ]))
        })
        .collect();

    state.select(Some(w.cursor.min(items.len().saturating_sub(1))));
    let mut b = block(&title);
    if w.filtering || !w.filter.is_empty() {
        b = b.title_bottom(Line::from(
            Span::raw(format!(" / {} ", w.filter)).fg(theme::ACCENT),
        ));
    }
    frame.render_stateful_widget(
        List::new(items)
            .block(b)
            .highlight_style(theme::selected())
            .highlight_symbol("› ")
            .highlight_spacing(ratatui::widgets::HighlightSpacing::Always),
        area,
        state,
    );
}

/// Abbreviate a home-relative path, so the column stays readable.
fn shorten(p: &str) -> String {
    let s = match std::env::var("HOME") {
        Ok(h) if p.starts_with(&h) => format!("~{}", &p[h.len()..]),
        _ => p.to_string(),
    };
    reporch_claude::events::truncate_chars(&s, 36)
}

fn consent(frame: &mut Frame, area: Rect, w: &Wizard, budget_usd_per_session: f64) {
    let text = crate::project::consent::consent_screen(
        &w.project_name,
        w.selected.len(),
        budget_usd_per_session,
    );
    frame.render_widget(
        Paragraph::new(text)
            .block(Block::default().padding(Padding::new(2, 2, 1, 0)))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn analysing(frame: &mut Frame, area: Rect, w: &Wizard) {
    // Real progress — repo name and outcome — not a spinner (§5.4).
    let lines: Vec<Line> = w
        .analysis
        .iter()
        .map(|(name, st)| match st {
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
            AnalysisState::Failed(e) => Line::from(vec![
                Span::raw(format!("  {name:<24}")),
                Span::raw(reporch_claude::events::truncate_chars(e, 48)).fg(theme::FAIL),
            ]),
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines).block(block("reading the repositories")),
        area,
    );
}

fn choose(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    help: &str,
    options: &[(String, String)],
    cursor: usize,
    state: &mut ListState,
) {
    let items: Vec<ListItem> = options
        .iter()
        .map(|(n, d)| {
            ListItem::new(Line::from(vec![
                Span::raw(format!("{n:<10}")),
                Span::raw(d.clone()).fg(theme::DIM),
            ]))
        })
        .collect();
    state.select(Some(cursor));
    frame.render_stateful_widget(
        List::new(items)
            .block(
                block(title)
                    .title_bottom(Line::from(Span::raw(format!(" {help} ")).fg(theme::DIM))),
            )
            .highlight_style(theme::selected())
            .highlight_symbol("› ")
            .highlight_spacing(ratatui::widgets::HighlightSpacing::Always),
        area,
        state,
    );
}

fn effort(frame: &mut Frame, area: Rect, w: &Wizard, state: &mut ListState) {
    let plan = w.effort_plan();
    // The ladder is shown, not just the choice, so the consequence for workers is visible.
    let options: Vec<(String, String)> = Effort::ALL
        .iter()
        .map(|e| {
            let worker = Effort::worker_effort(*e);
            (
                e.as_str().to_string(),
                format!("{} · workers run at {worker}", e.description()),
            )
        })
        .collect();
    let help = format!(
        "master {} · workers {}{}",
        plan.master,
        plan.worker,
        if w.ultracode {
            " · ultracode on (master only)"
        } else {
            ""
        }
    );
    choose(
        frame,
        area,
        "effort",
        &help,
        &options,
        w.effort_cursor,
        state,
    );
}

fn master(frame: &mut Frame, area: Rect, w: &Wizard) {
    let text = format!(
        "reporch suggests `{}` should lead this task.\n\n{}\n\n\
         Every repository still gets a worktree and a session — the master is the one that plans \
         the work and delegates it.\n\n\
         Use ↑↓ to choose a different one.",
        w.master, w.master_reason
    );
    frame.render_widget(
        Paragraph::new(text)
            .block(block("master repository"))
            .wrap(Wrap { trim: false }),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use reporch_core::{GraphNode, ProjectRepo};

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn project() -> ProjectBase {
        let mut g = std::collections::BTreeMap::new();
        g.insert(
            "modulex".to_string(),
            GraphNode {
                consumes: vec![],
                produces_contracts_for: vec!["modulex-ui".into()],
            },
        );
        ProjectBase {
            schema_version: 1,
            name: "modulex".into(),
            created_at: "t".into(),
            analyzed_at: None,
            overview: String::new(),
            repositories: ["modulex", "modulex-ui"]
                .iter()
                .map(|n| ProjectRepo {
                    name: n.to_string(),
                    path: format!("/r/{n}"),
                    remote_url: None,
                    default_branch: None,
                    purpose: String::new(),
                    profile_ref: None,
                    cross_session_inbound: None,
                })
                .collect(),
            graph: g,
            settings: Default::default(),
            next_task_id: 1,
        }
    }

    #[tokio::test]
    async fn an_analysis_job_always_emits_a_terminal_event() {
        let temp = tempfile::tempdir().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        run_analysis(
            AnalysisRequest {
                project_name: "empty".into(),
                selected: Vec::new(),
                paths: Paths::at(temp.path().join("home")),
                config: Config::default(),
                preserve: None,
            },
            tx,
        )
        .await;

        match rx.recv().await {
            Some(AnalysisEvent::Finished(result)) => match *result {
                Err(error) => {
                    assert!(error.contains("no repository could be analysed"), "{error}");
                }
                Ok(project) => panic!("expected failure, got project {project:?}"),
            },
            other => panic!("expected a terminal failure event, got {other:?}"),
        }
        assert!(rx.recv().await.is_none(), "one job has one terminal event");
    }

    fn render(w: &Wizard) -> String {
        let mut t = Terminal::new(TestBackend::new(96, 22)).unwrap();
        let mut st = ListState::default();
        t.draw(|f| draw(f, f.area(), w, &mut st, 10.0)).unwrap();
        t.backend().to_string()
    }

    #[test]
    fn naming_a_project_leads_to_a_scan() {
        let mut w = Wizard::new();
        for c in "modulex".chars() {
            on_key(&mut w, key(KeyCode::Char(c)), None);
        }
        assert_eq!(w.project_name, "modulex");
        assert_eq!(on_key(&mut w, key(KeyCode::Enter), None), Command::Scan);
        assert_eq!(w.step, Step::Scanning);
    }

    #[test]
    fn an_unnamed_project_cannot_advance() {
        let mut w = Wizard::new();
        assert_eq!(on_key(&mut w, key(KeyCode::Enter), None), Command::None);
        assert_eq!(w.step, Step::ProjectName);
        assert!(render(&w).contains("a project needs a name"));
    }

    /// §12.1: consent is actively confirmed. Enter must not do it — people press Enter by reflex.
    #[test]
    fn consent_requires_y_and_not_enter() {
        let mut w = Wizard::new();
        w.step = Step::Consent;
        assert_eq!(on_key(&mut w, key(KeyCode::Enter), None), Command::None);
        assert_eq!(w.step, Step::Consent, "Enter must not grant consent");

        assert_eq!(
            on_key(&mut w, key(KeyCode::Char('y')), None),
            Command::ConsentGiven
        );
        assert_eq!(w.step, Step::Analysing);
    }

    #[test]
    fn declining_consent_cancels() {
        let mut w = Wizard::new();
        w.step = Step::Consent;
        assert_eq!(
            on_key(&mut w, key(KeyCode::Char('n')), None),
            Command::Cancel
        );
    }

    #[test]
    fn the_consent_screen_shows_the_real_session_count() {
        let mut w = Wizard::new();
        w.project_name = "modulex".into();
        w.selected = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        w.step = Step::Consent;
        let s = render(&w);
        assert!(s.contains("bypassPermissions"), "{s}");
        assert!(s.contains("3 sessions will run at once"), "{s}");
        assert!(s.contains("y ] I understand"), "{s}");
    }

    #[test]
    fn the_brief_is_multi_line_and_finished_with_ctrl_d() {
        let mut w = Wizard::new();
        w.step = Step::TaskBrief;
        for c in "line one".chars() {
            on_key(&mut w, key(KeyCode::Char(c)), None);
        }
        on_key(&mut w, key(KeyCode::Enter), None);
        for c in "line two".chars() {
            on_key(&mut w, key(KeyCode::Char(c)), None);
        }
        assert_eq!(w.brief, "line one\nline two");
        assert_eq!(w.step, Step::TaskBrief, "Enter must not submit");

        on_key(
            &mut w,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
            None,
        );
        assert_eq!(w.step, Step::PickModel);
    }

    #[test]
    fn choosing_effort_shows_what_workers_will_run_at() {
        let mut w = Wizard::new();
        w.step = Step::PickEffort;
        w.effort_cursor = 4; // max
        let s = render(&w);
        assert!(s.contains("master max"), "{s}");
        assert!(
            s.contains("workers xhigh"),
            "the ladder's consequence must be visible: {s}"
        );

        on_key(&mut w, key(KeyCode::Char('u')), None);
        assert!(w.ultracode);
        assert!(render(&w).contains("ultracode on (master only)"));
    }

    #[test]
    fn confirming_effort_elects_a_master_from_the_graph() {
        let p = project();
        let mut w = Wizard::new();
        w.step = Step::PickEffort;
        on_key(&mut w, key(KeyCode::Enter), Some(&p));
        assert_eq!(w.step, Step::ConfirmMaster);
        assert_eq!(w.master, "modulex", "furthest upstream");
        assert!(w.master_reason.contains("upstream"));
    }

    /// §5.7: the election is editable before the task starts.
    #[test]
    fn the_master_can_be_changed_before_starting() {
        let p = project();
        let mut w = Wizard::new();
        w.step = Step::ConfirmMaster;
        w.master = "modulex".into();

        on_key(&mut w, key(KeyCode::Down), Some(&p));
        assert_eq!(w.master, "modulex-ui");
        let s = render(&w);
        assert!(s.contains("modulex-ui"), "{s}");
        assert!(s.contains("chosen by you"), "{s}");

        assert_eq!(
            on_key(&mut w, key(KeyCode::Enter), Some(&p)),
            Command::StartTask
        );
    }

    #[test]
    fn esc_backs_out_of_a_step() {
        let mut w = Wizard::new();
        w.step = Step::PickModel;
        assert_eq!(on_key(&mut w, key(KeyCode::Esc), None), Command::Cancel);
    }

    #[test]
    fn esc_while_filtering_clears_the_filter_rather_than_leaving() {
        let mut w = Wizard::new();
        w.step = Step::PickRepos;
        w.filtering = true;
        w.filter = "docs".into();
        assert_eq!(on_key(&mut w, key(KeyCode::Esc), None), Command::None);
        assert!(w.filter.is_empty());
        assert!(!w.filtering);
        assert_eq!(w.step, Step::PickRepos, "still in the picker");
    }

    #[test]
    fn the_picker_renders_checkboxes_and_the_count() {
        let mut w = Wizard::new();
        w.project_name = "modulex".into();
        w.candidates = vec![
            reporch_scan::FoundRepo {
                name: "modulex".into(),
                path: "/repos/modulex".into(),
                remote_url: None,
                remote_org: Some("example-org".into()),
                head_age_secs: None,
            },
            reporch_scan::FoundRepo {
                name: "modulex-ui".into(),
                path: "/repos/modulex-ui".into(),
                remote_url: None,
                remote_org: Some("example-org".into()),
                head_age_secs: None,
            },
        ];
        w.step = Step::PickRepos;
        w.rerank();
        on_key(&mut w, key(KeyCode::Char(' ')), None);

        let s = render(&w);
        assert!(s.contains("[x]"), "{s}");
        assert!(s.contains("[ ]"), "{s}");
        assert!(s.contains("1 selected"), "{s}");
        assert!(s.contains("example-org"), "the org column: {s}");
        assert!(s.contains("space select"), "{s}");
    }

    #[test]
    fn select_all_takes_everything_visible() {
        let mut w = Wizard::new();
        w.candidates = (0..4)
            .map(|i| reporch_scan::FoundRepo {
                name: format!("r{i}"),
                path: format!("/r/r{i}").into(),
                remote_url: None,
                remote_org: None,
                head_age_secs: None,
            })
            .collect();
        w.step = Step::PickRepos;
        w.rerank();
        on_key(&mut w, key(KeyCode::Char('a')), None);
        assert_eq!(w.selected.len(), 4);
    }
}
