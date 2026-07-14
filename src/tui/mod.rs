pub mod data;
pub mod input;
pub mod state;
pub mod ui;

use anyhow::{Context, Result};
use ratatui::crossterm::event::{self, Event as CtEvent};
use std::io::IsTerminal;

pub fn run(cfg: crate::config::Config, db_path: String) -> Result<()> {
    anyhow::ensure!(
        std::io::stdout().is_terminal(),
        "the tui needs an interactive terminal: run it via 'docker compose exec -it vaultkeeper vaultkeeper tui'"
    );
    let hub = data::DataHub::new(cfg, db_path)?;
    let mut app = state::App::new();
    if let Ok(ev) = hub.refresh() {
        apply(&mut app, ev);
    }

    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app, &hub);
    ratatui::restore();
    result
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut state::App,
    hub: &data::DataHub,
) -> Result<()> {
    let mut last_refresh = std::time::Instant::now();
    loop {
        terminal
            .draw(|f| ui::render(f, app))
            .context("draw failed")?;
        while let Some(ev) = hub.try_recv() {
            apply(app, ev);
        }
        if last_refresh.elapsed() > std::time::Duration::from_secs(2) {
            if let Ok(ev) = hub.refresh() {
                apply(app, ev);
            }
            last_refresh = std::time::Instant::now();
        }
        if event::poll(std::time::Duration::from_millis(200))? {
            if let CtEvent::Key(key) = event::read()? {
                if key.kind != event::KeyEventKind::Press {
                    continue;
                }
                if let Some(cmd) = app.handle_key(key) {
                    match cmd {
                        state::Command::Quit => return Ok(()),
                        state::Command::Refresh => {
                            if let Ok(ev) = hub.refresh() {
                                apply(app, ev);
                            }
                        }
                        other => dispatch(app, hub, other),
                    }
                }
            }
        }
    }
}

fn apply(app: &mut state::App, ev: data::Event) {
    match ev {
        data::Event::Refreshed { sources, runs } => {
            app.sources = sources;
            app.runs = runs;
        }
        data::Event::Snapshots { source, snapshots } => {
            app.snapshots = snapshots;
            app.snapshots_for = Some(source);
        }
        data::Event::ActionDone(msg) => {
            app.busy.retain(|b| *b != msg);
            app.status_line = format!("done: {msg}");
        }
        data::Event::ActionFailed(msg) => {
            app.status_line = format!("FAILED: {msg}");
            app.busy.clear();
        }
    }
}

/// Wiring point for Tasks 4-6: Quit and Refresh are handled in the event
/// loop above (they don't need `hub` beyond a synchronous refresh), so
/// every remaining `Command` variant lands here. Task 4 wires
/// LoadSnapshots/RunBackup/RunVerify onto spawned workers, Task 5 wires
/// SaveSource/SetEnabled, Task 6 wires Restore.
fn dispatch(app: &mut state::App, hub: &data::DataHub, cmd: state::Command) {
    let _ = (app, hub, cmd);
}
