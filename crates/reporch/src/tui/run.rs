//! The task run screen (§5.8).
//!
//! ```text
//!   TASK-0001  widget-status                    running · 6m12s · $1.84 / $10.00
//!
//!   REPO                  ROLE     STATE      LAST                                  BRIEFS
//!   example-app               master   working    editing app/widgets.py              ─
//!   example-app-ui            worker   working    reading brief 0003                    1 in
//!   example-app-python        worker   done       replied to brief 0002                 1 in 1 out
//!
//!   enter attach · b briefs · l logs · p pause · x abort
//! ```
//!
//! A pure projection of [`RunView`], which the supervisor owns. The screen holds nothing.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, Table, TableState};
use ratatui::Frame;

use reporch_core::RepoState;
use reporch_store::time::human_duration;

use crate::task::RunView;
use crate::tui::theme;

/// Colour for a repo's state — the run screen's fastest signal.
fn state_style(s: RepoState) -> Style {
    match s {
        RepoState::Working => Style::default().fg(theme::ACCENT),
        RepoState::Done => Style::default().fg(theme::OK),
        RepoState::Blocked | RepoState::Dead => Style::default().fg(theme::FAIL),
        RepoState::Waiting => Style::default().fg(theme::WARN),
        RepoState::Spawning | RepoState::Idle => Style::default().fg(theme::DIM),
    }
}

/// The BRIEFS column: `1 in 1 out`, or `─` when nothing has moved.
fn briefs_cell(in_: usize, out: usize) -> String {
    match (in_, out) {
        (0, 0) => "─".to_string(),
        (i, 0) => format!("{i} in"),
        (0, o) => format!("{o} out"),
        (i, o) => format!("{i} in {o} out"),
    }
}

/// Draw the run screen.
pub struct RunMeta<'a> {
    pub task_id: &'a str,
    pub slug: &'a str,
    pub elapsed_secs: u64,
    pub confirm_abort: bool,
}

pub fn draw(
    frame: &mut Frame,
    area: Rect,
    meta: RunMeta<'_>,
    view: &RunView,
    table: &mut TableState,
) {
    let [header, body, notices, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(4),
        Constraint::Length(1),
    ])
    .areas(area);

    // ---- header: what is running, for how long, and what it has cost -------
    let status = match &view.finished {
        None => Span::raw("running").fg(theme::ACCENT),
        Some(crate::task::FinishReason::Completed) => Span::raw("completed").fg(theme::OK),
        Some(crate::task::FinishReason::Aborted(_)) => Span::raw("aborted").fg(theme::FAIL),
        Some(crate::task::FinishReason::Cancelled) => Span::raw("cancelled").fg(theme::WARN),
        Some(crate::task::FinishReason::Delayed(_)) => Span::raw("delayed").fg(theme::WARN),
    };
    // Spend is shown against the worst-case task ceiling. An unbriefed worker costs zero because
    // Claude's paid startup charge is lazy until its first turn (§10 Deviation).
    let spend = format!("${:.2} / ${:.2}", view.spent_usd, view.budget_usd);
    let spend_style = if view.budget_usd > 0.0 && view.spent_usd / view.budget_usd > 0.8 {
        Style::default().fg(theme::WARN)
    } else {
        Style::default().fg(theme::DIM)
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("  "),
            Span::raw(meta.task_id).bold(),
            Span::raw("  "),
            Span::raw(meta.slug).fg(theme::ACCENT),
            Span::raw("    "),
            status,
            Span::raw(" · ").fg(theme::DIM),
            Span::raw(human_duration(meta.elapsed_secs)).fg(theme::DIM),
            Span::raw(" · ").fg(theme::DIM),
            Span::styled(spend, spend_style),
        ])),
        header,
    );

    // ---- the repo table ----------------------------------------------------
    let rows: Vec<Row> = view
        .rows
        .iter()
        .map(|r| {
            Row::new(vec![
                Cell::from(r.repo.clone()),
                Cell::from(r.role.as_str()).style(Style::default().fg(theme::DIM)),
                Cell::from(r.state.as_str()).style(state_style(r.state)),
                Cell::from(reporch_claude::events::truncate_chars(&r.last, 44)),
                Cell::from(briefs_cell(r.briefs_in, r.briefs_out))
                    .style(Style::default().fg(theme::DIM)),
                Cell::from(format!("${:.2}", r.spent_usd)).style(Style::default().fg(theme::DIM)),
            ])
        })
        .collect();

    let table_widget = Table::new(
        rows,
        [
            Constraint::Length(21),
            Constraint::Length(8),
            Constraint::Length(10),
            // Fill on the widest column, so LAST takes whatever is left.
            Constraint::Fill(1),
            Constraint::Length(14),
            Constraint::Length(8),
        ],
    )
    .header(
        Row::new(vec!["REPO", "ROLE", "STATE", "LAST", "BRIEFS", "COST"])
            .style(Style::default().fg(theme::DIM)),
    )
    .row_highlight_style(theme::selected())
    .highlight_symbol("› ");

    frame.render_stateful_widget(
        table_widget,
        body.inner(ratatui::layout::Margin::new(2, 0)),
        table,
    );

    // ---- notices: pings, violations, deaths --------------------------------
    let recent: Vec<Line> = view
        .notices
        .iter()
        .rev()
        .take(3)
        .rev()
        .map(|n| {
            let style = if n.contains("violation") || n.contains("aborted") {
                Style::default().fg(theme::FAIL)
            } else {
                Style::default().fg(theme::DIM)
            };
            Line::from(vec![Span::raw("  "), Span::styled(n.clone(), style)])
        })
        .collect();
    frame.render_widget(Paragraph::new(recent), notices);

    let footer_text = if meta.confirm_abort {
        "  Abort every session and preserve all worktrees? y confirm · n/esc cancel"
    } else if view.finished.is_some() {
        "  enter attach · b briefs · l logs · esc project"
    } else if view.paused {
        "  enter attach · b briefs · l logs · p resume attention · x abort"
    } else {
        "  enter attach · b briefs · l logs · p pause attention · x abort"
    };
    let footer_style = if meta.confirm_abort {
        Style::default().fg(theme::WARN)
    } else {
        Style::default().fg(theme::DIM)
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::raw(footer_text)).style(footer_style)),
        footer,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{FinishReason, RunRow};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use reporch_core::Role;

    fn view() -> RunView {
        RunView {
            rows: vec![
                RunRow {
                    repo: "example-app".into(),
                    role: Role::Master,
                    state: RepoState::Working,
                    last: "editing app/widgets.py".into(),
                    briefs_in: 0,
                    briefs_out: 0,
                    spent_usd: 0.9,
                    token_usage: Default::default(),
                },
                RunRow {
                    repo: "example-app-ui".into(),
                    role: Role::Worker,
                    state: RepoState::Working,
                    last: "reading brief 0003".into(),
                    briefs_in: 1,
                    briefs_out: 0,
                    spent_usd: 0.5,
                    token_usage: Default::default(),
                },
                RunRow {
                    repo: "example-app-python".into(),
                    role: Role::Worker,
                    state: RepoState::Done,
                    last: "replied to brief 0002".into(),
                    briefs_in: 1,
                    briefs_out: 1,
                    spent_usd: 0.44,
                    token_usage: Default::default(),
                },
            ],
            spent_usd: 1.84,
            budget_usd: 10.0,
            token_usage: Default::default(),
            notices: vec!["`example-app-ui` pinged by reporch — brief 0001 filed".into()],
            finished: None,
            paused: false,
        }
    }

    fn render(v: &RunView) -> String {
        let mut t = Terminal::new(TestBackend::new(100, 16)).unwrap();
        let mut state = TableState::default();
        t.draw(|f| {
            draw(
                f,
                f.area(),
                RunMeta {
                    task_id: "TASK-0001",
                    slug: "widget-status",
                    elapsed_secs: 372,
                    confirm_abort: false,
                },
                v,
                &mut state,
            )
        })
        .unwrap();
        t.backend().to_string()
    }

    /// The §5.8 layout, asserted against a real rendered buffer.
    #[test]
    fn it_renders_the_spec_screen() {
        let s = render(&view());
        assert!(s.contains("TASK-0001"), "{s}");
        assert!(s.contains("widget-status"), "{s}");
        assert!(s.contains("running"), "{s}");
        assert!(s.contains("6m12s"), "the §5.8 elapsed example: {s}");
        assert!(s.contains("$1.84 / $10.00"), "{s}");
        for h in ["REPO", "ROLE", "STATE", "LAST", "BRIEFS", "COST"] {
            assert!(s.contains(h), "missing header {h}: {s}");
        }
        assert!(s.contains("example-app-python"), "{s}");
        assert!(s.contains("editing app/widgets.py"), "{s}");
        assert!(s.contains("1 in 1 out"), "{s}");
        assert!(s.contains("$0.44"), "per-repository spend is visible: {s}");
        assert!(s.contains("enter attach"), "{s}");
    }

    #[test]
    fn brief_traffic_reads_naturally() {
        assert_eq!(briefs_cell(0, 0), "─");
        assert_eq!(briefs_cell(1, 0), "1 in");
        assert_eq!(briefs_cell(0, 2), "2 out");
        assert_eq!(briefs_cell(1, 1), "1 in 1 out");
    }

    #[test]
    fn a_finished_task_says_so_in_the_header() {
        let mut v = view();
        v.finished = Some(FinishReason::Completed);
        let completed = render(&v);
        assert!(completed.contains("completed"));
        assert!(completed.contains("esc project"), "{completed}");

        v.finished = Some(FinishReason::Aborted("ping loop".into()));
        assert!(render(&v).contains("aborted"));
    }

    #[test]
    fn a_long_last_column_is_truncated_not_wrapped() {
        let mut v = view();
        v.rows[0].last = "x".repeat(300);
        let s = render(&v);
        // 16 rows at 100 cols; a wrap would push the footer off the screen.
        assert!(s.contains("enter attach"), "the footer must survive: {s}");
    }

    /// Repo names and tool inputs can contain wide characters; slicing bytes would panic.
    #[test]
    fn multibyte_content_does_not_panic() {
        let mut v = view();
        v.rows[0].repo = "リポジトリ".into();
        v.rows[0].last = "編集中 app/認証/keys.py ".repeat(10);
        let _ = render(&v);
    }

    #[test]
    fn an_empty_task_still_renders() {
        let v = RunView {
            rows: vec![],
            spent_usd: 0.0,
            budget_usd: 10.0,
            token_usage: Default::default(),
            notices: vec![],
            finished: None,
            paused: false,
        };
        let s = render(&v);
        assert!(s.contains("REPO"), "{s}");
    }

    #[test]
    fn only_the_most_recent_notices_are_shown() {
        let mut v = view();
        v.notices = (0..20).map(|i| format!("notice-{i}")).collect();
        let s = render(&v);
        assert!(s.contains("notice-19"), "the newest must be visible: {s}");
        assert!(
            !s.contains("notice-0 "),
            "the oldest must have scrolled off: {s}"
        );
    }
}
