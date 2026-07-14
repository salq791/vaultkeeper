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
            // Snapshot loads don't go through ActionDone (they carry a
            // payload, not just a status string), so this arm has to clear
            // the "snapshots <name>" busy label itself or it would linger
            // forever after a successful load.
            let label = data::action_label("snapshots", &source);
            app.busy.retain(|b| *b != label);
            app.status_line = format!("done: {label}");
            app.snapshots = snapshots;
            app.snapshots_for = Some(source);
        }
        data::Event::ActionDone { label, message } => {
            app.busy.retain(|b| *b != label);
            app.status_line = format!("done: {message}");
        }
        data::Event::ActionFailed { label, message } => {
            // Retain on the label only: a failure must not wipe other
            // actions' busy entries, which are still genuinely in flight.
            app.busy.retain(|b| *b != label);
            app.status_line = format!("FAILED: {message}");
        }
    }
}

/// Wiring point for Tasks 4-6: Quit and Refresh are handled in the event
/// loop above (they don't need `hub` beyond a synchronous refresh), so
/// every remaining `Command` variant lands here. Task 4 wires
/// LoadSnapshots/RunBackup/RunVerify onto spawned workers (busy label +
/// status line + a `DataHub` call, never blocking this thread); Task 5
/// wires SaveSource/SetEnabled, Task 6 wires Restore, so those three arms
/// stay intentionally empty for now.
fn dispatch(app: &mut state::App, hub: &data::DataHub, cmd: state::Command) {
    match cmd {
        state::Command::LoadSnapshots(name) => {
            let label = data::action_label("snapshots", &name);
            app.busy.push(label.clone());
            app.status_line = format!("running: {label}");
            hub.load_snapshots(name);
        }
        state::Command::RunBackup(name) => {
            let label = data::action_label("backup", &name);
            app.busy.push(label.clone());
            app.status_line = format!("running: {label}");
            hub.spawn_backup(name);
        }
        state::Command::RunVerify(name) => {
            let label = data::action_label("verify", &name);
            app.busy.push(label.clone());
            app.status_line = format!("running: {label}");
            hub.spawn_verify(name);
        }
        // Deferred to Task 6: restore confirmation.
        state::Command::Restore { .. } => {}
        // Deferred to Task 5: source add/edit form save.
        state::Command::SaveSource { .. } => {}
        // Deferred to Task 5: enable/disable toggle.
        state::Command::SetEnabled(..) => {}
        // Matched in the event loop above; never reaches dispatch.
        state::Command::Quit | state::Command::Refresh => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use state::{App, Command};

    /// Config with just enough fields set to construct a `DataHub`; the
    /// worker threads these tests provoke will fail fast (bad db path/repo)
    /// and report `ActionFailed` on the channel, which is fine since these
    /// tests only assert the synchronous busy/status effects `dispatch`
    /// applies before ever spawning a thread.
    fn test_cfg() -> crate::config::Config {
        crate::config::Config {
            global: crate::config::Global {
                staging_dir: std::path::PathBuf::from("does-not-exist-staging"),
                restic_repo: "local:does-not-exist-repo".into(),
                restic_password: "test".into(),
                restic_timeout_minutes: None,
            },
            notify: Default::default(),
            verify: Default::default(),
        }
    }

    #[test]
    fn dispatch_run_backup_pushes_busy_label_and_status() {
        let hub = data::DataHub::new(test_cfg(), "does-not-exist.db".into()).unwrap();
        let mut app = App::new();
        dispatch(&mut app, &hub, Command::RunBackup("a-db".into()));
        assert_eq!(app.busy, vec!["backup a-db".to_string()]);
        assert_eq!(app.status_line, "running: backup a-db");
    }

    #[test]
    fn dispatch_run_verify_pushes_busy_label_and_status() {
        let hub = data::DataHub::new(test_cfg(), "does-not-exist.db".into()).unwrap();
        let mut app = App::new();
        dispatch(&mut app, &hub, Command::RunVerify("a-db".into()));
        assert_eq!(app.busy, vec!["verify a-db".to_string()]);
        assert_eq!(app.status_line, "running: verify a-db");
    }

    #[test]
    fn dispatch_load_snapshots_pushes_busy_label_and_status() {
        let hub = data::DataHub::new(test_cfg(), "does-not-exist.db".into()).unwrap();
        let mut app = App::new();
        dispatch(&mut app, &hub, Command::LoadSnapshots("a-db".into()));
        assert_eq!(app.busy, vec!["snapshots a-db".to_string()]);
        assert_eq!(app.status_line, "running: snapshots a-db");
    }

    #[test]
    fn dispatch_deferred_commands_are_noop() {
        let hub = data::DataHub::new(test_cfg(), "does-not-exist.db".into()).unwrap();
        let mut app = App::new();
        dispatch(&mut app, &hub, Command::SetEnabled("a-db".into(), false));
        assert!(app.busy.is_empty());
        assert!(app.status_line.is_empty());
    }

    #[test]
    fn apply_action_done_clears_exactly_its_busy_label() {
        let mut app = App::new();
        app.busy.push("backup a-db".to_string());
        app.busy.push("verify other".to_string());
        apply(
            &mut app,
            data::Event::ActionDone {
                label: "backup a-db".into(),
                message: "backup a-db: success".into(),
            },
        );
        assert_eq!(app.busy, vec!["verify other".to_string()]);
        assert_eq!(app.status_line, "done: backup a-db: success");
    }

    #[test]
    fn apply_action_failed_clears_only_its_own_label() {
        let mut app = App::new();
        app.busy.push("backup a-db".to_string());
        app.busy.push("verify other".to_string());
        apply(
            &mut app,
            data::Event::ActionFailed {
                label: "backup a-db".into(),
                message: "backup a-db: boom".into(),
            },
        );
        assert_eq!(app.busy, vec!["verify other".to_string()]);
        assert_eq!(app.status_line, "FAILED: backup a-db: boom");
    }

    #[test]
    fn apply_snapshots_clears_matching_busy_label_only() {
        let mut app = App::new();
        app.busy.push("snapshots a-db".to_string());
        app.busy.push("backup other".to_string());
        apply(
            &mut app,
            data::Event::Snapshots {
                source: "a-db".into(),
                snapshots: vec![],
            },
        );
        assert_eq!(app.busy, vec!["backup other".to_string()]);
        assert_eq!(app.snapshots_for, Some("a-db".to_string()));
    }
}
