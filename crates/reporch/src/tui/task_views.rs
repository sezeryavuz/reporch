//! Read-only task diagnostics for the run screen (§5.8, §8.3, §10).
//!
//! These views reopen the persisted files on every draw. That makes them live without sharing
//! session state with the UI, and keeps briefs strictly data: their markdown is displayed
//! verbatim and never fed back into a session.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use reporch_core::brief::IndexEntry;
use reporch_core::Task;
use reporch_store::{atomic, mailbox, TaskPaths};

use super::theme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticEntry {
    pub heading: String,
    pub detail: String,
}

/// Load one session's complete persisted stream. A partial final write is still displayed as raw
/// text instead of making the viewer unavailable.
pub fn session_log(paths: &TaskPaths, repo: &str) -> Vec<DiagnosticEntry> {
    let path = paths.session_log(repo);
    let Ok(text) = atomic::read_string(&path) else {
        return vec![DiagnosticEntry {
            heading: format!("{repo} · no persisted output yet"),
            detail: format!("Waiting for {}", path.display()),
        }];
    };
    text.lines()
        .enumerate()
        .map(|(line, raw)| DiagnosticEntry {
            heading: format!("{:>5}  {}", line + 1, stream_summary(raw)),
            detail: raw.to_string(),
        })
        .collect()
}

/// Load every persisted stream, grouped by repository. This is intentionally a raw task log, not
/// a synthetic global chronology: Claude's stream lines carry no trustworthy cross-process clock.
pub fn task_logs(paths: &TaskPaths, task: &Task) -> Vec<DiagnosticEntry> {
    let mut entries = Vec::new();
    for repo in &task.repositories {
        for mut entry in session_log(paths, &repo.name) {
            entry.heading = format!("{} · {}", repo.name, entry.heading);
            entries.push(entry);
        }
    }
    entries
}

/// Load briefs and pings in the exact append order of `briefs/index.json` (§8.3).
pub fn mailbox_timeline(paths: &TaskPaths) -> Vec<DiagnosticEntry> {
    let Ok(index) = mailbox::load_index(paths) else {
        return vec![DiagnosticEntry {
            heading: "mailbox could not be read".into(),
            detail: paths.brief_index().display().to_string(),
        }];
    };
    if index.entries.is_empty() {
        return vec![DiagnosticEntry {
            heading: "mailbox is empty".into(),
            detail: "Briefs and control-plane pings will appear here in append order.".into(),
        }];
    }

    index
        .entries
        .iter()
        .map(|entry| match entry {
            IndexEntry::Brief(brief) => {
                let detail = atomic::read_string(&paths.brief_file(&brief.file))
                    .unwrap_or_else(|error| format!("Could not read {}: {error}", brief.file));
                DiagnosticEntry {
                    heading: format!(
                        "{}  {} → {}  [{}]  {}",
                        brief.id, brief.from, brief.to, brief.status, brief.subject
                    ),
                    detail,
                }
            }
            IndexEntry::Ping(ping) => DiagnosticEntry {
                heading: format!("{}  ping → {} by {:?}", ping.at, ping.to, ping.by),
                detail: format!(
                    "Attention event\n\nTo: {}\nBy: {:?}\nAt: {}\nReason: {}",
                    ping.to, ping.by, ping.at, ping.reason
                ),
            },
        })
        .collect()
}

fn stream_summary(raw: &str) -> String {
    use reporch_claude::events::StreamEvent;

    let timestamp = serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .and_then(|value| {
            value
                .get("timestamp")
                .or_else(|| value.get("created_at"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        });
    let summary = match reporch_claude::events::parse_line(raw) {
        Some(StreamEvent::Init(init)) => format!("init · {} · {}", init.model, init.session_id),
        Some(StreamEvent::Status { status }) => format!("status · {status}"),
        Some(StreamEvent::CompactBoundary { trigger, .. }) => format!("compact · {trigger}"),
        Some(StreamEvent::Hook { phase, name }) => format!("hook · {phase:?} · {name}"),
        Some(StreamEvent::Assistant(message)) => message
            .tool_uses
            .first()
            .map(|tool| format!("{} · {}", tool.name, tool.summary))
            .unwrap_or_else(|| {
                let text = message.text.trim();
                if text.is_empty() {
                    "assistant · thinking".into()
                } else {
                    format!(
                        "assistant · {}",
                        reporch_claude::events::truncate_chars(text, 72)
                    )
                }
            }),
        Some(StreamEvent::User(message)) if message.is_replay => {
            "injected turn acknowledged".into()
        }
        Some(StreamEvent::User(message)) => {
            format!("tool results · {}", message.tool_result_count)
        }
        Some(StreamEvent::PartialText { text }) => format!(
            "partial · {}",
            reporch_claude::events::truncate_chars(text.trim(), 72)
        ),
        Some(StreamEvent::Result(result)) => format!(
            "result · {} · ${:.4}",
            result.subtype, result.total_cost_usd
        ),
        Some(StreamEvent::RateLimit { status, .. }) => format!("rate limit · {status}"),
        Some(StreamEvent::Unknown { kind, .. }) => format!("{kind} · raw event"),
        None => "blank".into(),
    };
    timestamp.map_or(summary.clone(), |time| format!("{time} · {summary}"))
}

/// Draw a selectable timeline beside the full raw entry.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    entries: &[DiagnosticEntry],
    cursor: usize,
    detail_scroll: u16,
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
            Span::raw(title).bold().fg(theme::ACCENT),
            Span::raw(format!("  · {} entries", entries.len())).fg(theme::DIM),
        ])),
        header,
    );

    let [timeline, detail] =
        Layout::horizontal([Constraint::Percentage(44), Constraint::Percentage(56)]).areas(body);
    let items = entries
        .iter()
        .map(|entry| ListItem::new(entry.heading.clone()))
        .collect::<Vec<_>>();
    let mut state = ListState::default();
    state.select((!entries.is_empty()).then_some(cursor.min(entries.len() - 1)));
    frame.render_stateful_widget(
        List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" timeline "))
            .highlight_symbol("› ")
            .highlight_style(theme::selected()),
        timeline,
        &mut state,
    );

    let selected = entries
        .get(cursor.min(entries.len().saturating_sub(1)))
        .map(|entry| entry.detail.as_str())
        .unwrap_or("No entries.");
    frame.render_widget(
        Paragraph::new(selected)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" persisted data "),
            )
            .wrap(Wrap { trim: false })
            .scroll((detail_scroll, 0)),
        detail,
    );
    frame.render_widget(
        Paragraph::new(
            Line::from(Span::raw(
                "  ↑↓ entry · pgup/pgdn detail · b briefs · l all logs · esc run",
            ))
            .fg(theme::DIM),
        ),
        footer,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use reporch_core::brief::BriefKind;

    #[test]
    fn a_real_jsonl_file_is_summarised_and_kept_verbatim() {
        let temp = tempfile::tempdir().unwrap();
        let paths = TaskPaths::at(temp.path());
        std::fs::create_dir_all(paths.logs_dir()).unwrap();
        let raw = r#"{"type":"result","subtype":"success","total_cost_usd":0.42}"#;
        atomic::write_string(&paths.session_log("api"), &format!("{raw}\n")).unwrap();

        let entries = session_log(&paths, "api");

        assert_eq!(entries.len(), 1);
        assert!(entries[0].heading.contains("result · success · $0.4200"));
        assert_eq!(entries[0].detail, raw);
    }

    #[test]
    fn the_mailbox_preserves_brief_and_ping_append_order_with_the_body() {
        let temp = tempfile::tempdir().unwrap();
        let paths = TaskPaths::at(temp.path());
        mailbox::send(
            &paths,
            "TASK-0001",
            "api",
            "ui",
            BriefKind::Request,
            "Add field",
            "The full untrusted body.",
            None,
            "2026-08-12T00:00:00Z",
        )
        .unwrap();
        mailbox::record_ping(
            &paths,
            "ui",
            reporch_core::brief::PingBy::ControlPlane,
            "brief 0001 filed",
            "2026-08-12T00:00:01Z",
        )
        .unwrap();

        let entries = mailbox_timeline(&paths);

        assert_eq!(entries.len(), 2);
        assert!(entries[0].heading.contains("0001  api → ui"));
        assert!(entries[0].detail.contains("The full untrusted body."));
        assert!(entries[1].heading.contains("ping → ui"));
        assert!(entries[1].detail.contains("brief 0001 filed"));
    }
}
