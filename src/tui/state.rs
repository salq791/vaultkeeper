use crate::tui::input::TextField;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::Color;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Dashboard,
    History,
    Sources,
    Snapshots,
}

impl Tab {
    fn next(self) -> Tab {
        match self {
            Tab::Dashboard => Tab::History,
            Tab::History => Tab::Sources,
            Tab::Sources => Tab::Snapshots,
            Tab::Snapshots => Tab::Dashboard,
        }
    }

    fn prev(self) -> Tab {
        match self {
            Tab::Dashboard => Tab::Snapshots,
            Tab::History => Tab::Dashboard,
            Tab::Sources => Tab::History,
            Tab::Snapshots => Tab::Sources,
        }
    }
}

/// Distinguishes a busy entry's kind for future UI treatment (e.g. a
/// different spinner label per action). `App::busy` stays `Vec<String>` per
/// this task's frozen contract; Tasks 4-6 may key off this enum when they
/// start populating `busy`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ActionKind {
    Backup,
    Verify,
}

/// Draft state for the add/edit source form. Only `Mode::SourceForm`'s
/// existence is part of this task's frozen contract; `SourceForm`'s own
/// field layout is this implementer's judgment call for Task 5 to build on
/// and adjust as needed.
#[allow(dead_code)]
pub struct SourceForm {
    pub editing: Option<String>,
    pub name: TextField,
    pub engine: TextField,
    pub schedule: TextField,
    pub verify_schedule: TextField,
    pub retention: TextField,
    pub healthchecks_uuid: TextField,
    pub verify_healthchecks_uuid: TextField,
    pub settings_json: TextField,
    pub secrets_json: TextField,
    pub keep_secrets: bool,
    pub focus: usize,
}

pub enum Mode {
    Browse,
    // Consumed by plan-6 Task 5 (source add/edit form).
    #[allow(dead_code)]
    SourceForm(Box<SourceForm>),
    // Consumed by plan-6 Task 6 (restore target entry).
    #[allow(dead_code)]
    RestoreTarget {
        snapshot_id: String,
        field: TextField,
    },
    // Consumed by plan-6 Task 6 (restore confirmation).
    #[allow(dead_code)]
    ConfirmRestore {
        snapshot_id: String,
        target: Option<String>,
        typed: TextField,
    },
    Help,
}

pub enum Command {
    // Consumed by later plan-6 tasks (manual refresh keybinding); the
    // periodic auto-refresh in mod.rs's event loop calls DataHub::refresh
    // directly rather than routing through this variant.
    #[allow(dead_code)]
    Refresh,
    Quit,
    // Consumed by plan-6 Task 4 (Enter on the Snapshots tab).
    #[allow(dead_code)]
    LoadSnapshots(String),
    RunBackup(String),
    RunVerify(String),
    // Consumed by plan-6 Task 6 (confirm restore).
    #[allow(dead_code)]
    Restore {
        source: String,
        snapshot: String,
        target: Option<String>,
    },
    // Consumed by plan-6 Task 5 (save source form).
    #[allow(dead_code)]
    SaveSource {
        draft: crate::store::NewSource,
        editing: Option<String>,
        keep_secrets: bool,
    },
    // Consumed by plan-6 Task 5 (enable/disable toggle).
    #[allow(dead_code)]
    SetEnabled(String, bool),
}

/// Manual `Debug` impl: `Command` is logged/matched around the TUI, but a
/// `Restore` command's `target` is `Option<String>` and may carry a
/// password-bearing database URL. Every other field here is a name, id, or
/// flag with no secret material, so only `target` needs redaction.
impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Command::Refresh => write!(f, "Refresh"),
            Command::Quit => write!(f, "Quit"),
            Command::LoadSnapshots(source) => f.debug_tuple("LoadSnapshots").field(source).finish(),
            Command::RunBackup(source) => f.debug_tuple("RunBackup").field(source).finish(),
            Command::RunVerify(source) => f.debug_tuple("RunVerify").field(source).finish(),
            Command::Restore {
                source,
                snapshot,
                target,
            } => f
                .debug_struct("Restore")
                .field("source", source)
                .field("snapshot", snapshot)
                .field("target", &target.as_ref().map(|_| "<target set>"))
                .finish(),
            Command::SaveSource {
                draft,
                editing,
                keep_secrets,
            } => f
                .debug_struct("SaveSource")
                .field("draft_name", &draft.name)
                .field("editing", editing)
                .field("keep_secrets", keep_secrets)
                .finish(),
            Command::SetEnabled(name, enabled) => f
                .debug_tuple("SetEnabled")
                .field(name)
                .field(enabled)
                .finish(),
        }
    }
}

pub struct App {
    pub tab: Tab,
    pub mode: Mode,
    pub sources: Vec<crate::store::SourceMeta>,
    pub sel_source: usize,
    pub runs: Vec<crate::store::RunView>,
    pub sel_run: usize,
    pub snapshots: Vec<crate::restic::Snapshot>,
    pub sel_snapshot: usize,
    pub snapshots_for: Option<String>,
    pub busy: Vec<String>,
    pub status_line: String,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> App {
        App {
            tab: Tab::Dashboard,
            mode: Mode::Browse,
            sources: Vec::new(),
            sel_source: 0,
            runs: Vec::new(),
            sel_run: 0,
            snapshots: Vec::new(),
            sel_snapshot: 0,
            snapshots_for: None,
            busy: Vec::new(),
            status_line: String::new(),
        }
    }

    pub fn selected_source(&self) -> Option<&crate::store::SourceMeta> {
        self.sources.get(self.sel_source)
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Command> {
        match self.mode {
            Mode::Browse => self.handle_browse_key(key),
            Mode::Help => {
                if matches!(key.code, KeyCode::Char('?') | KeyCode::Esc) {
                    self.mode = Mode::Browse;
                }
                None
            }
            // Task 5 wires source-form key handling (text entry, save/cancel).
            Mode::SourceForm(_) => None,
            // Task 6 wires restore-target text entry.
            Mode::RestoreTarget { .. } => None,
            // Task 6 wires restore confirm/cancel.
            Mode::ConfirmRestore { .. } => None,
        }
    }

    fn handle_browse_key(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Char('q') => Some(Command::Quit),
            KeyCode::Char('?') => {
                self.mode = Mode::Help;
                None
            }
            KeyCode::Tab => {
                self.tab = self.tab.next();
                None
            }
            KeyCode::BackTab => {
                self.tab = self.tab.prev();
                None
            }
            KeyCode::Down => {
                self.move_selection(1);
                None
            }
            KeyCode::Up => {
                self.move_selection(-1);
                None
            }
            KeyCode::Char('r') => self.run_action(false),
            KeyCode::Char('v') => self.run_action(true),
            _ => None,
        }
    }

    fn move_selection(&mut self, delta: i64) {
        match self.tab {
            Tab::Dashboard | Tab::Sources => {
                Self::move_index(&mut self.sel_source, self.sources.len(), delta)
            }
            Tab::History => Self::move_index(&mut self.sel_run, self.runs.len(), delta),
            Tab::Snapshots => Self::move_index(&mut self.sel_snapshot, self.snapshots.len(), delta),
        }
    }

    fn move_index(sel: &mut usize, len: usize, delta: i64) {
        if len == 0 {
            *sel = 0;
            return;
        }
        let max = (len - 1) as i64;
        let next = (*sel as i64 + delta).clamp(0, max);
        *sel = next as usize;
    }

    fn run_action(&self, verify: bool) -> Option<Command> {
        if !matches!(self.tab, Tab::Dashboard | Tab::Sources) {
            return None;
        }
        let name = self.selected_source()?.name.clone();
        Some(if verify {
            Command::RunVerify(name)
        } else {
            Command::RunBackup(name)
        })
    }
}

/// Fails closed to gray for any status string this task doesn't recognize,
/// so a future/unknown status never silently reads as "fine" (green).
pub fn status_color(status: &str) -> Color {
    match status {
        "success" => Color::Green,
        "verify_passed" => Color::Cyan,
        "success_prune_failed" => Color::Yellow,
        "failed" | "verify_failed" => Color::Red,
        _ => Color::Gray,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEvent};

    pub(crate) fn meta(name: &str) -> crate::store::SourceMeta {
        crate::store::SourceMeta {
            id: 1,
            name: name.into(),
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
            enabled: true,
        }
    }

    #[test]
    fn tab_cycles_and_q_quits() {
        let mut app = App::new();
        assert!(matches!(app.tab, Tab::Dashboard));
        assert!(app.handle_key(KeyEvent::from(KeyCode::Tab)).is_none());
        assert!(matches!(app.tab, Tab::History));
        assert!(matches!(
            app.handle_key(KeyEvent::from(KeyCode::Char('q'))),
            Some(Command::Quit)
        ));
    }

    #[test]
    fn run_key_targets_selected_source() {
        let mut app = App::new();
        app.sources = vec![meta("a-db"), meta("b-db")];
        app.handle_key(KeyEvent::from(KeyCode::Down));
        match app.handle_key(KeyEvent::from(KeyCode::Char('r'))) {
            Some(Command::RunBackup(n)) => assert_eq!(n, "b-db"),
            other => panic!("expected RunBackup, got {other:?}"),
        }
    }

    #[test]
    fn selection_clamps_to_list() {
        let mut app = App::new();
        app.sources = vec![meta("only")];
        for _ in 0..5 {
            app.handle_key(KeyEvent::from(KeyCode::Down));
        }
        assert_eq!(app.sel_source, 0);
    }

    #[test]
    fn status_colors_fail_closed_to_gray() {
        use ratatui::style::Color;
        assert_eq!(status_color("success"), Color::Green);
        assert_eq!(status_color("failed"), Color::Red);
        assert_eq!(status_color("verify_failed"), Color::Red);
        assert_eq!(status_color("success_prune_failed"), Color::Yellow);
        assert_eq!(status_color("verify_passed"), Color::Cyan);
        assert_eq!(status_color("some_future_status"), Color::Gray);
    }

    #[test]
    fn restore_command_debug_redacts_target() {
        let c = Command::Restore {
            source: "a-db".into(),
            snapshot: "snap1".into(),
            target: Some("postgres://u:secretpw@h/db".into()),
        };
        let dbg = format!("{c:?}");
        assert!(!dbg.contains("secretpw"));
        assert!(dbg.contains("a-db"));
    }
}
