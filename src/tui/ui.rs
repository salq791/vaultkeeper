use crate::tui::state::{status_color, App, Mode, Tab};
use chrono::Local;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Tabs},
    Frame,
};

const TAB_TITLES: [&str; 4] = ["Dashboard", "History", "Sources", "Snapshots"];

/// Draws the tabs bar, the active tab's body, and the status line; overlays
/// a help screen on top when `Mode::Help` is active.
pub fn render(f: &mut Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

    render_tabs(f, app, chunks[0]);
    match app.tab {
        Tab::Dashboard => render_dashboard(f, app, chunks[1]),
        Tab::History => render_history(f, app, chunks[1]),
        Tab::Sources => render_sources(f, app, chunks[1]),
        Tab::Snapshots => render_snapshots(f, app, chunks[1]),
    }
    render_status_line(f, app, chunks[2]);

    if matches!(app.mode, Mode::Help) {
        render_help_overlay(f, area);
    }
}

fn tab_index(tab: Tab) -> usize {
    match tab {
        Tab::Dashboard => 0,
        Tab::History => 1,
        Tab::Sources => 2,
        Tab::Snapshots => 3,
    }
}

fn render_tabs(f: &mut Frame, app: &App, area: Rect) {
    let titles: Vec<Line> = TAB_TITLES.iter().map(|t| Line::from(*t)).collect();
    let tabs = Tabs::new(titles)
        .block(Block::default().borders(Borders::ALL).title("vaultkeeper"))
        .select(tab_index(app.tab))
        .highlight_style(
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::White),
        );
    f.render_widget(tabs, area);
}

/// The newest run for `name`: `app.runs` is populated from
/// `recent_runs_view`, which orders newest-first, so the first match is the
/// latest.
fn latest_run_for<'a>(
    runs: &'a [crate::store::RunView],
    name: &str,
) -> Option<&'a crate::store::RunView> {
    runs.iter().find(|r| r.source == name)
}

fn render_dashboard(f: &mut Frame, app: &App, area: Rect) {
    let header = Row::new(vec![
        "Name",
        "Engine",
        "Schedule",
        "Enabled",
        "Last status",
        "Last run",
        "Next run",
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = app
        .sources
        .iter()
        .map(|s| {
            let last = latest_run_for(&app.runs, &s.name);
            let last_status = last.map(|r| r.status.as_str()).unwrap_or("-");
            let last_run = last.map(|r| r.started_at.as_str()).unwrap_or("-");
            let next_run = match crate::schedule::next_occurrence(&s.schedule, Local::now()) {
                Ok(t) => t.format("%Y-%m-%d %H:%M").to_string(),
                Err(_) => "?".to_string(),
            };
            Row::new(vec![
                Cell::from(s.name.clone()),
                Cell::from(s.engine.clone()),
                Cell::from(s.schedule.clone()),
                Cell::from(if s.enabled { "yes" } else { "no" }),
                Cell::from(last_status.to_string())
                    .style(Style::default().fg(status_color(last_status))),
                Cell::from(last_run.to_string()),
                Cell::from(next_run),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(20),
        Constraint::Length(10),
        Constraint::Length(14),
        Constraint::Length(9),
        Constraint::Length(14),
        Constraint::Length(20),
        Constraint::Min(16),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title("Dashboard"))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    let mut state = TableState::default();
    state.select(Some(app.sel_source));
    f.render_stateful_widget(table, area, &mut state);
}

fn render_history(f: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(area);

    let header = Row::new(vec!["Source", "Kind", "Status", "Started", "Finished"])
        .style(Style::default().add_modifier(Modifier::BOLD));
    let rows: Vec<Row> = app
        .runs
        .iter()
        .map(|r| {
            Row::new(vec![
                Cell::from(r.source.clone()),
                Cell::from(r.kind.clone()),
                Cell::from(r.status.clone()).style(Style::default().fg(status_color(&r.status))),
                Cell::from(r.started_at.clone()),
                Cell::from(r.finished_at.clone().unwrap_or_else(|| "-".to_string())),
            ])
        })
        .collect();
    let widths = [
        Constraint::Length(20),
        Constraint::Length(10),
        Constraint::Length(14),
        Constraint::Length(20),
        Constraint::Length(20),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title("History"))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    let mut state = TableState::default();
    state.select(Some(app.sel_run));
    f.render_stateful_widget(table, chunks[0], &mut state);

    let detail = app
        .runs
        .get(app.sel_run)
        .and_then(|r| r.detail.as_deref())
        .unwrap_or("(no detail)");
    let detail_widget =
        Paragraph::new(detail).block(Block::default().borders(Borders::ALL).title("Detail"));
    f.render_widget(detail_widget, chunks[1]);
}

/// Full source detail (not just the Dashboard's operational summary): this
/// is the one place Task 3 surfaces `SourceMeta`'s schedule/retention/
/// healthchecks fields, since the Dashboard tab is deliberately limited to
/// the brief's operational-status column set.
fn render_sources(f: &mut Frame, app: &App, area: Rect) {
    let header = Row::new(vec![
        "ID",
        "Name",
        "Engine",
        "Schedule",
        "Verify sched",
        "Retention d/w/m",
        "Healthchecks",
        "Enabled",
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));
    let rows: Vec<Row> = app
        .sources
        .iter()
        .map(|s| {
            let retention = format!(
                "{}/{}/{}",
                s.retention.daily, s.retention.weekly, s.retention.monthly
            );
            let healthchecks = match (&s.healthchecks_uuid, &s.verify_healthchecks_uuid) {
                (Some(_), Some(_)) => "backup+verify",
                (Some(_), None) => "backup",
                (None, Some(_)) => "verify",
                (None, None) => "-",
            };
            Row::new(vec![
                Cell::from(s.id.to_string()),
                Cell::from(s.name.clone()),
                Cell::from(s.engine.clone()),
                Cell::from(s.schedule.clone()),
                Cell::from(s.verify_schedule.clone().unwrap_or_else(|| "-".to_string())),
                Cell::from(retention),
                Cell::from(healthchecks),
                Cell::from(if s.enabled { "yes" } else { "no" }),
            ])
        })
        .collect();
    let widths = [
        Constraint::Length(4),
        Constraint::Length(20),
        Constraint::Length(10),
        Constraint::Length(14),
        Constraint::Length(14),
        Constraint::Length(17),
        Constraint::Length(14),
        Constraint::Length(9),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title("Sources"))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    let mut state = TableState::default();
    state.select(Some(app.sel_source));
    f.render_stateful_widget(table, area, &mut state);
}

/// First 8 characters of a restic snapshot id (its usual short form).
/// Char-based rather than a byte slice so this never panics on an id
/// shorter than 8 bytes.
fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// `app.snapshots` is populated by `Command::LoadSnapshots` dispatching to
/// `DataHub::load_snapshots`, which sorts newest first; this just renders
/// whatever's currently in state (empty until that load completes).
fn render_snapshots(f: &mut Frame, app: &App, area: Rect) {
    let header =
        Row::new(vec!["ID", "Time", "Tags"]).style(Style::default().add_modifier(Modifier::BOLD));
    let rows: Vec<Row> = app
        .snapshots
        .iter()
        .map(|s| {
            Row::new(vec![
                Cell::from(short_id(&s.id)),
                Cell::from(s.time.clone()),
                Cell::from(s.tags.join(",")),
            ])
        })
        .collect();
    let widths = [
        Constraint::Length(20),
        Constraint::Length(24),
        Constraint::Min(16),
    ];
    let title = match &app.snapshots_for {
        Some(name) => format!("Snapshots ({name})"),
        None => "Snapshots (select a source and press Enter)".to_string(),
    };
    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    let mut state = TableState::default();
    state.select(Some(app.sel_snapshot));
    f.render_stateful_widget(table, area, &mut state);
}

/// Busy labels (in-flight backup/verify/snapshot loads) take priority over
/// the last status message so an in-progress action stays visible until its
/// own outcome event retains it out of `busy` (each action clears exactly
/// its own label); falls back to the keybinding hint when nothing is busy
/// and no status has been set yet.
fn render_status_line(f: &mut Frame, app: &App, area: Rect) {
    let text = if !app.busy.is_empty() {
        format!("busy: {}", app.busy.join(", "))
    } else if app.status_line.is_empty() {
        "q: quit  Tab: next  Shift+Tab: prev  Up/Down: select  r: backup  v: verify  ?: help"
            .to_string()
    } else {
        app.status_line.clone()
    };
    f.render_widget(Paragraph::new(text), area);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

fn render_help_overlay(f: &mut Frame, area: Rect) {
    let popup = centered_rect(60, 60, area);
    let lines = [
        "q         quit",
        "Tab       next tab",
        "Shift+Tab previous tab",
        "Up/Down   move selection",
        "r         run backup on selected source",
        "v         run verify on selected source",
        "?         toggle this help",
    ]
    .join("\n");
    f.render_widget(Clear, popup);
    let block = Block::default().borders(Borders::ALL).title("Help");
    f.render_widget(Paragraph::new(lines).block(block), popup);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn dashboard_renders_source_rows() {
        let mut app = crate::tui::state::App::new();
        app.sources = vec![crate::tui::state::tests::meta("render-me")];
        let backend = TestBackend::new(100, 24);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| render(f, &app)).unwrap();
        let text = format!("{:?}", term.backend().buffer());
        assert!(text.contains("render-me"));
    }

    #[test]
    fn short_id_takes_first_eight_chars() {
        assert_eq!(short_id("0123456789abcdef"), "01234567");
        assert_eq!(short_id("abc"), "abc");
    }

    #[test]
    fn snapshots_tab_renders_short_id_time_and_tags() {
        let mut app = crate::tui::state::App::new();
        app.tab = Tab::Snapshots;
        app.snapshots_for = Some("acme-db".to_string());
        app.snapshots = vec![crate::restic::Snapshot {
            id: "0123456789abcdef0123456789abcdef".to_string(),
            time: "2026-07-13T22:00:00Z".to_string(),
            tags: vec!["source=acme-db".to_string()],
        }];
        let backend = TestBackend::new(100, 24);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| render(f, &app)).unwrap();
        let text = format!("{:?}", term.backend().buffer());
        assert!(text.contains("01234567"));
        assert!(!text.contains("0123456789abcdef0123456789abcdef"));
        assert!(text.contains("2026-07-13T22:00:00Z"));
        assert!(text.contains("source=acme-db"));
    }

    #[test]
    fn status_line_shows_busy_labels_over_status_text() {
        let mut app = crate::tui::state::App::new();
        app.busy = vec!["backup acme-db".to_string()];
        app.status_line = "done: verify other-db".to_string();
        let backend = TestBackend::new(100, 24);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| render(f, &app)).unwrap();
        let text = format!("{:?}", term.backend().buffer());
        assert!(text.contains("busy: backup acme-db"));
    }

    #[test]
    fn dashboard_running_status_renders_gray() {
        let mut app = crate::tui::state::App::new();
        app.sources = vec![crate::tui::state::tests::meta("run-me")];
        app.runs = vec![crate::store::RunView {
            source: "run-me".to_string(),
            kind: "backup".to_string(),
            status: "running".to_string(),
            started_at: "2026-07-13T00:00:00Z".to_string(),
            finished_at: None,
            detail: None,
        }];
        let backend = TestBackend::new(100, 24);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| render(f, &app)).unwrap();
        let buf = term.backend().buffer();
        let mut found = false;
        for y in 0..buf.area.height {
            let row: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
            if let Some(idx) = row.find("running") {
                assert_eq!(buf[(idx as u16, y)].fg, Color::Gray);
                found = true;
                break;
            }
        }
        assert!(
            found,
            "expected 'running' status text in the dashboard render"
        );
    }
}
