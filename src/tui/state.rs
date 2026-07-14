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

/// Field labels for `SourceForm::fields`, in the exact pinned order the
/// contract requires: index is load-bearing (tests and `validate` both index
/// positionally), and the trailing `secrets_json` entry is the only masked
/// field.
const SOURCE_FORM_LABELS: [&str; 9] = [
    "name",
    "engine",
    "schedule",
    "verify_schedule",
    "retention",
    "healthchecks_uuid",
    "verify_healthchecks_uuid",
    "settings_json",
    "secrets_json",
];

/// Draft state for the add/edit source form. `fields` holds the nine
/// label/value pairs above in pinned order; `editing` names the source being
/// edited (`None` for a fresh add); `focus` indexes the field currently
/// receiving keystrokes.
pub struct SourceForm {
    pub editing: Option<String>,
    pub focus: usize,
    pub fields: Vec<(String, TextField)>,
}

impl SourceForm {
    pub fn new_add() -> SourceForm {
        let fields = SOURCE_FORM_LABELS
            .iter()
            .map(|label| {
                (
                    (*label).to_string(),
                    TextField::new(*label == "secrets_json"),
                )
            })
            .collect();
        SourceForm {
            editing: None,
            focus: 0,
            fields,
        }
    }

    /// Pre-fills every field from `meta` except secrets, which always stays
    /// empty: secret material is write-only from the TUI's perspective and
    /// never round-trips back into the form for display.
    pub fn new_edit(meta: &crate::store::SourceMeta) -> SourceForm {
        let mut form = SourceForm::new_add();
        form.editing = Some(meta.name.clone());
        form.fields[0].1.set(&meta.name);
        form.fields[1].1.set(&meta.engine);
        form.fields[2].1.set(&meta.schedule);
        form.fields[3]
            .1
            .set(meta.verify_schedule.as_deref().unwrap_or(""));
        form.fields[4].1.set(&format!(
            "{},{},{}",
            meta.retention.daily, meta.retention.weekly, meta.retention.monthly
        ));
        form.fields[5]
            .1
            .set(meta.healthchecks_uuid.as_deref().unwrap_or(""));
        form.fields[6]
            .1
            .set(meta.verify_healthchecks_uuid.as_deref().unwrap_or(""));
        form.fields[7]
            .1
            .set(&serde_json::to_string(&meta.settings).unwrap_or_else(|_| "{}".to_string()));
        // fields[8] (secrets_json) intentionally left empty.
        form
    }

    /// Builds a `NewSource` from the current field values, and reports
    /// whether the caller should keep the source's existing secrets blob
    /// (true iff this is an edit and the secrets field was left blank).
    pub fn validate(&self) -> anyhow::Result<(crate::store::NewSource, bool)> {
        use anyhow::Context;

        let name = self.fields[0].1.value().to_string();
        crate::store::validate_name(&name)?;

        let engine = self.fields[1].1.value().to_string();
        crate::engines::engine_for(&engine)?;

        let schedule = self.fields[2].1.value().to_string();
        crate::schedule::validate(&schedule)?;

        let verify_schedule = Self::non_empty(self.fields[3].1.value());
        if let Some(vs) = &verify_schedule {
            crate::schedule::validate(vs)?;
        }

        let retention = crate::types::Retention::parse_csv(self.fields[4].1.value())?;

        let healthchecks_uuid = Self::non_empty(self.fields[5].1.value());
        let verify_healthchecks_uuid = Self::non_empty(self.fields[6].1.value());

        let settings_raw = self.fields[7].1.value();
        let settings: serde_json::Value = if settings_raw.trim().is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_str(settings_raw).context("invalid settings JSON")?
        };

        // One blankness predicate shared by keep_secrets AND the map branch:
        // if these ever diverge (e.g. is_empty vs trim().is_empty()), a
        // whitespace-only secrets field on edit reseals an empty map over
        // the stored credentials, silently destroying them.
        let secrets_raw = self.fields[8].1.value();
        let secrets_blank = secrets_raw.trim().is_empty();
        let keep_secrets = self.editing.is_some() && secrets_blank;
        let secrets: std::collections::HashMap<String, String> = if secrets_blank {
            std::collections::HashMap::new()
        } else {
            serde_json::from_str(secrets_raw).context("invalid secrets JSON")?
        };

        Ok((
            crate::store::NewSource {
                name,
                engine,
                schedule,
                verify_schedule,
                retention,
                healthchecks_uuid,
                verify_healthchecks_uuid,
                settings,
                secrets,
            },
            keep_secrets,
        ))
    }

    /// Whitespace-only means "unset", same blankness rule as the secrets
    /// field above, so an accidental space never turns an optional field
    /// into `Some("  ")` junk.
    fn non_empty(s: &str) -> Option<String> {
        if s.trim().is_empty() {
            None
        } else {
            Some(s.to_string())
        }
    }
}

pub enum Mode {
    Browse,
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
    SaveSource {
        draft: crate::store::NewSource,
        editing: Option<String>,
        keep_secrets: bool,
    },
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
            Mode::SourceForm(_) => self.handle_source_form_key(key),
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
            KeyCode::Enter => self.enter_action(),
            KeyCode::Char('a') => self.open_add_form(),
            KeyCode::Char('e') => self.open_edit_form(),
            KeyCode::Char('d') => self.toggle_enabled(),
            _ => None,
        }
    }

    /// Routes keys while `Mode::SourceForm` is active: Esc cancels back to
    /// Browse without touching the form; Up/Down move focus between the nine
    /// fields; Enter validates and either produces `Command::SaveSource`
    /// (closing the form) or leaves the form open with the error on the
    /// status line; every other key is forwarded to the focused field so
    /// typing/backspace edit it.
    fn handle_source_form_key(&mut self, key: KeyEvent) -> Option<Command> {
        if matches!(key.code, KeyCode::Esc) {
            self.mode = Mode::Browse;
            return None;
        }
        let Mode::SourceForm(form) = &mut self.mode else {
            return None;
        };
        match key.code {
            KeyCode::Up => {
                form.focus = form.focus.saturating_sub(1);
                None
            }
            KeyCode::Down => {
                if form.focus + 1 < form.fields.len() {
                    form.focus += 1;
                }
                None
            }
            KeyCode::Enter => match form.validate() {
                Ok((draft, keep_secrets)) => {
                    let editing = form.editing.clone();
                    self.mode = Mode::Browse;
                    Some(Command::SaveSource {
                        draft,
                        editing,
                        keep_secrets,
                    })
                }
                Err(e) => {
                    self.status_line = format!("{e:#}");
                    None
                }
            },
            _ => {
                form.fields[form.focus].1.handle(key);
                None
            }
        }
    }

    /// Opens a blank add form; only meaningful on the Sources tab.
    fn open_add_form(&mut self) -> Option<Command> {
        if !matches!(self.tab, Tab::Sources) {
            return None;
        }
        self.mode = Mode::SourceForm(Box::new(SourceForm::new_add()));
        None
    }

    /// Opens an edit form pre-filled from the currently selected source;
    /// only meaningful on the Sources tab, and a no-op with no selection.
    fn open_edit_form(&mut self) -> Option<Command> {
        if !matches!(self.tab, Tab::Sources) {
            return None;
        }
        let form = SourceForm::new_edit(self.selected_source()?);
        self.mode = Mode::SourceForm(Box::new(form));
        None
    }

    /// `d` on the Sources tab flips the selected source's enabled flag.
    fn toggle_enabled(&self) -> Option<Command> {
        if !matches!(self.tab, Tab::Sources) {
            return None;
        }
        let s = self.selected_source()?;
        Some(Command::SetEnabled(s.name.clone(), !s.enabled))
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

    /// Rider (3): dedupes against `busy` the same way `enter_action` already
    /// does for snapshot loads, so a held-down `r`/`v` never queues a second
    /// backup/verify worker for a source whose run is still in flight.
    fn run_action(&self, verify: bool) -> Option<Command> {
        if !matches!(self.tab, Tab::Dashboard | Tab::Sources) {
            return None;
        }
        let name = self.selected_source()?.name.clone();
        let kind = if verify { "verify" } else { "backup" };
        if self
            .busy
            .contains(&crate::tui::data::action_label(kind, &name))
        {
            return None;
        }
        Some(if verify {
            Command::RunVerify(name)
        } else {
            Command::RunBackup(name)
        })
    }

    /// Enter on the Snapshots tab loads the currently selected source's
    /// snapshot list (the selection persists across tabs, same as
    /// `run_action`'s use of `selected_source`). Skips reissuing the command
    /// when `app.snapshots_for` already names this source (data already on
    /// screen) or when its `action_label` is still in `busy` (a load is
    /// already in flight), so repeated Enter never spawns duplicate load
    /// workers.
    fn enter_action(&self) -> Option<Command> {
        if !matches!(self.tab, Tab::Snapshots) {
            return None;
        }
        let name = self.selected_source()?.name.clone();
        if self.snapshots_for.as_deref() == Some(name.as_str()) {
            return None;
        }
        if self
            .busy
            .contains(&crate::tui::data::action_label("snapshots", &name))
        {
            return None;
        }
        Some(Command::LoadSnapshots(name))
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
    fn enter_on_snapshots_tab_loads_selected_source() {
        let mut app = App::new();
        app.sources = vec![meta("a-db")];
        app.tab = Tab::Snapshots;
        match app.handle_key(KeyEvent::from(KeyCode::Enter)) {
            Some(Command::LoadSnapshots(n)) => assert_eq!(n, "a-db"),
            other => panic!("expected LoadSnapshots, got {other:?}"),
        }
    }

    #[test]
    fn enter_on_snapshots_tab_skips_reload_when_already_loaded() {
        let mut app = App::new();
        app.sources = vec![meta("a-db")];
        app.tab = Tab::Snapshots;
        app.snapshots_for = Some("a-db".to_string());
        assert!(app.handle_key(KeyEvent::from(KeyCode::Enter)).is_none());
    }

    #[test]
    fn double_enter_on_snapshots_tab_is_noop_while_load_in_flight() {
        let mut app = App::new();
        app.sources = vec![meta("a-db")];
        app.tab = Tab::Snapshots;
        // First Enter dispatched a load: its busy label is still in flight
        // and no snapshots have arrived yet (snapshots_for is still None),
        // so a second Enter must not spawn a duplicate load worker.
        app.busy
            .push(crate::tui::data::action_label("snapshots", "a-db"));
        assert!(app.handle_key(KeyEvent::from(KeyCode::Enter)).is_none());
    }

    #[test]
    fn enter_key_outside_snapshots_tab_is_noop() {
        let mut app = App::new();
        app.sources = vec![meta("a-db")];
        assert!(matches!(app.tab, Tab::Dashboard));
        assert!(app.handle_key(KeyEvent::from(KeyCode::Enter)).is_none());
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

    // --- Plan 6 Task 5: source management ---

    #[test]
    fn a_opens_add_form_and_esc_cancels() {
        let mut app = App::new();
        app.tab = Tab::Sources;
        assert!(app.handle_key(KeyEvent::from(KeyCode::Char('a'))).is_none());
        assert!(matches!(app.mode, Mode::SourceForm(_)));
        assert!(app.handle_key(KeyEvent::from(KeyCode::Esc)).is_none());
        assert!(matches!(app.mode, Mode::Browse));
    }

    #[test]
    fn edit_prefills_all_but_secrets() {
        let form = SourceForm::new_edit(&meta("a-db"));
        assert_eq!(form.editing.as_deref(), Some("a-db"));
        assert_eq!(form.fields[0].1.value(), "a-db");
        assert_eq!(form.fields[4].1.value(), "7,4,6");
        assert_eq!(
            form.fields[8].1.value(),
            "",
            "secrets never round-trip into the form"
        );
        assert!(form.fields[8].1.masked);
    }

    #[test]
    fn validate_keep_secrets_semantics() {
        let mut form = SourceForm::new_edit(&meta("a-db"));
        let (_, keep) = form.validate().unwrap();
        assert!(keep, "empty secrets on edit keeps the blob");
        form.fields[8].1.set(r#"{"password":"new"}"#);
        let (draft, keep) = form.validate().unwrap();
        assert!(!keep);
        assert_eq!(draft.secrets.get("password").unwrap(), "new");
    }

    // Review fix: keep_secrets and the map-building branch must share ONE
    // blankness predicate. A whitespace-only secrets field on edit used to
    // fail `is_empty()` (keep_secrets = false) while passing
    // `trim().is_empty()` (empty map), silently resealing an empty map over
    // the stored credentials.
    #[test]
    fn whitespace_only_secrets_on_edit_keeps_blob() {
        let mut form = SourceForm::new_edit(&meta("a-db"));
        form.fields[8].1.set("   ");
        let (draft, keep) = form.validate().unwrap();
        assert!(
            keep,
            "whitespace-only secrets on edit must keep the stored blob"
        );
        assert!(draft.secrets.is_empty());
    }

    // Same inconsistency class for the optional fields: whitespace-only
    // must mean "unset" (None), not a Some("  ") that then fails cron
    // validation (verify_schedule) or lands as junk (healthchecks uuids).
    #[test]
    fn whitespace_only_optional_fields_become_none() {
        let mut form = SourceForm::new_edit(&meta("a-db"));
        form.fields[3].1.set("  ");
        form.fields[5].1.set("  ");
        let (draft, _) = form.validate().unwrap();
        assert!(draft.verify_schedule.is_none());
        assert!(draft.healthchecks_uuid.is_none());
    }

    #[test]
    fn validate_rejects_bad_schedule_and_engine() {
        let mut form = SourceForm::new_add();
        form.fields[0].1.set("x-db");
        form.fields[1].1.set("postgres");
        form.fields[2].1.set("banana");
        assert!(form.validate().is_err());
        form.fields[2].1.set("0 2 * * *");
        form.fields[1].1.set("nosuchengine");
        assert!(form.validate().is_err());
    }

    #[test]
    fn d_toggles_enabled() {
        let mut app = App::new();
        app.tab = Tab::Sources;
        app.sources = vec![meta("a-db")];
        match app.handle_key(KeyEvent::from(KeyCode::Char('d'))) {
            Some(Command::SetEnabled(n, en)) => {
                assert_eq!(n, "a-db");
                assert!(!en);
            }
            other => panic!("expected SetEnabled, got {other:?}"),
        }
    }

    // Rider (3): r/v must dedupe against `busy` the same way Enter already
    // does on the Snapshots tab, so a held-down key never queues a second
    // backup/verify worker for a source whose run is still in flight.
    #[test]
    fn double_press_run_or_verify_is_noop_while_busy() {
        let mut app = App::new();
        app.sources = vec![meta("a-db")];
        app.busy
            .push(crate::tui::data::action_label("backup", "a-db"));
        assert!(app.handle_key(KeyEvent::from(KeyCode::Char('r'))).is_none());
        app.busy.clear();
        app.busy
            .push(crate::tui::data::action_label("verify", "a-db"));
        assert!(app.handle_key(KeyEvent::from(KeyCode::Char('v'))).is_none());
    }
}
