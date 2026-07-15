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
    let timezone = cfg
        .global
        .timezone
        .parse::<chrono_tz::Tz>()
        .with_context(|| format!("invalid IANA timezone '{}'", cfg.global.timezone))?;
    let hub = data::DataHub::new(cfg, db_path)?;
    let mut app = state::App::new();
    app.timezone = timezone;
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
/// every remaining `Command` variant lands here. LoadSnapshots/RunBackup/
/// RunVerify/Restore dispatch onto spawned workers (busy label + status
/// line + a `DataHub` call, never blocking this thread); SaveSource/
/// SetEnabled run synchronously instead (fast SQLite writes with no worker
/// thread needed).
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
        state::Command::Restore {
            source,
            snapshot,
            target,
        } => {
            let label = data::action_label("restore", &source);
            app.busy.push(label.clone());
            app.status_line = format!("running: {label}");
            hub.spawn_restore(source, snapshot, target);
        }
        // Runs synchronously (fast SQLite op, no worker thread): no busy
        // label, status line and source-list refresh happen immediately.
        state::Command::SaveSource {
            draft,
            editing,
            keep_secrets,
        } => match hub.save_source(&draft, &editing, keep_secrets) {
            Ok(msg) => {
                app.status_line = format!("done: {msg}");
                if let Ok(ev) = hub.refresh() {
                    apply(app, ev);
                }
            }
            Err(e) => app.status_line = format!("FAILED: {e:#}"),
        },
        // Same synchronous shape as SaveSource above.
        state::Command::SetEnabled(name, enabled) => match hub.set_enabled(&name, enabled) {
            Ok(msg) => {
                app.status_line = format!("done: {msg}");
                if let Ok(ev) = hub.refresh() {
                    apply(app, ev);
                }
            }
            Err(e) => app.status_line = format!("FAILED: {e:#}"),
        },
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
                secret_temp_dir: std::path::PathBuf::from("does-not-exist-secrets"),
                restore_output_dir: std::path::PathBuf::from("does-not-exist-restores"),
                restic_repo: "local:does-not-exist-repo".into(),
                restic_password: "test".into(),
                restic_host: "vaultkeeper-test".into(),
                restic_timeout_minutes: None,
                maintenance_schedule: "0 3 * * 0".into(),
                timezone: "UTC".into(),
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
    fn dispatch_restore_pushes_busy_label_and_status() {
        let hub = data::DataHub::new(test_cfg(), "does-not-exist.db".into()).unwrap();
        let mut app = App::new();
        dispatch(
            &mut app,
            &hub,
            Command::Restore {
                source: "a-db".into(),
                snapshot: "snap1".into(),
                target: None,
            },
        );
        assert_eq!(app.busy, vec!["restore a-db".to_string()]);
        assert_eq!(app.status_line, "running: restore a-db");
    }

    /// SaveSource/SetEnabled run synchronously (no worker thread, no busy
    /// label): unlike RunBackup/RunVerify/LoadSnapshots above, `dispatch`
    /// itself performs the SQLite write and sets `status_line` directly.
    /// There's no source named "a-db" in this freshly-created test db, so
    /// this deterministically takes the error path regardless of whether
    /// VAULTKEEPER_MASTER_KEY happens to be set in the test environment.
    #[test]
    fn dispatch_set_enabled_runs_synchronously_with_no_busy_label() {
        let hub = data::DataHub::new(test_cfg(), "does-not-exist-set-enabled.db".into()).unwrap();
        let mut app = App::new();
        dispatch(&mut app, &hub, Command::SetEnabled("a-db".into(), false));
        assert!(
            app.busy.is_empty(),
            "sync source-management actions never use busy labels"
        );
        assert!(
            app.status_line.starts_with("FAILED:"),
            "no source named a-db exists in the fresh test db: {}",
            app.status_line
        );
    }

    #[test]
    fn dispatch_save_source_runs_synchronously_with_no_busy_label() {
        let hub = data::DataHub::new(test_cfg(), "does-not-exist-save-source.db".into()).unwrap();
        let mut app = App::new();
        let draft = crate::store::NewSource {
            name: "a-db".into(),
            engine: "postgres".into(),
            schedule: "0 2 * * *".into(),
            verify_schedule: None,
            retention: crate::types::Retention {
                daily: 7,
                weekly: 4,
                monthly: 6,
            },
            healthchecks_uuid: None,
            verify_healthchecks_uuid: None,
            settings: serde_json::json!({}),
            secrets: std::collections::HashMap::new(),
        };
        dispatch(
            &mut app,
            &hub,
            Command::SaveSource {
                draft,
                editing: None,
                keep_secrets: false,
            },
        );
        assert!(
            app.busy.is_empty(),
            "sync source-management actions never use busy labels"
        );
        assert!(
            app.status_line.starts_with("FAILED:") || app.status_line.starts_with("done:"),
            "unexpected status line: {}",
            app.status_line
        );
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
