# Vaultkeeper Plan 6: Full-Control TUI + Launch Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `docker compose exec -it vaultkeeper vaultkeeper tui` opens a keyboard-driven dashboard where an operator watches runs, manages sources and encrypted credentials, browses snapshots, and triggers backup, verify, and restore (restore gated by snapshot selection plus typed-name confirmation), and the repo carries launch-ready docs.

**Architecture:** The TUI is a pure-state core (`state.rs`, fully unit-tested: every keypress maps to an optional `Command`), a data layer (`data.rs`: quick synchronous SQLite refreshes plus background `std::thread` workers that reuse `exec::execute_source`/`execute_verify`/`execute_restore` and send `Event`s over an mpsc channel), and a render layer (`ui.rs`, smoke-tested with ratatui's TestBackend). The journal remains the single source of truth: actions spawn threads and the 2-second refresh shows `running` rows, so TUI state can never disagree with reality. Secrets never enter screen state: source lists use a new decryption-free `list_sources_meta`, and credential form fields are write-only and masked.

**Tech Stack:** ratatui (crossterm backend) as the only new dependency. Carried mandates land first: per-source `verify_healthchecks_uuid` (verify runs stop polluting the backup dead-man switch), timeout validation in check-config, `build_repo` reuse, and the image/docs minors.

**Spec:** `docs/superpowers/specs/2026-07-13-vaultkeeper-design.md` (TUI section: full control including restore, typed-name confirmation, masked credentials, docker exec usage, restore-point selection from the Snapshots screen). Plan 6 of 6: the finish line.

## Global Constraints

- PUBLIC REPO: no secrets, tokens, real hostnames, or real project refs in ANY committed file.
- Never use em dashes in any file, code comment, or doc. Use commas, colons, or hyphens.
- `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` must pass at every commit.
- Secrets NEVER in TUI screen state, logs, or Debug surfaces: source listings use `list_sources_meta` (no decryption); the secrets form field is masked and write-only; editing a source with the secrets field left empty keeps the existing sealed blob untouched.
- Destructive TUI actions (restore) require selecting a concrete snapshot and typing the source name exactly; run/verify are non-destructive and fire directly.
- The TUI performs no work on the render thread besides SQLite reads: restic and engine work happens on worker threads reporting via events.
- Conventional commit messages with trailer: Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
- TDD with REAL captured RED output for every pure-logic change; render smoke tests use ratatui TestBackend; the interactive terminal loop itself is disclosed untested glue.
- Windows dev note: the TUI compiles and runs in a real terminal on Windows; automated tests never require a TTY.

---

### Task 1: Store plumbing + verify healthchecks separation (mandate)

**Files:**
- Modify: `src/store.rs`, `src/types.rs`, `src/exec.rs`, `src/main.rs`
- Test: inline in `src/store.rs`, `src/types.rs`; one tests/cli.rs addition

**Interfaces:**
- Consumes: existing store/exec.
- Produces (the TUI tasks build on ALL of these exact signatures):
  - Migration: `sources` gains column `verify_healthchecks_uuid TEXT` (pragma-guarded `ALTER TABLE` so existing databases upgrade in place); `NewSource` and `SourceRow` gain `pub verify_healthchecks_uuid: Option<String>`.
  - `exec::execute_verify` notifies healthchecks with `source.verify_healthchecks_uuid` (NOT the backup uuid); when unset, verify sends NO healthchecks ping (webhook/SES behavior unchanged). Rationale comment: the backup check measures backup freshness; verify polluting it defeats the dead-man switch.
  - CLI: `source add --verify-healthchecks-uuid <U>` (optional).
  - `store::SourceMeta { pub id: i64, pub name: String, pub engine: String, pub schedule: String, pub verify_schedule: Option<String>, pub retention: Retention, pub healthchecks_uuid: Option<String>, pub verify_healthchecks_uuid: Option<String>, pub settings: serde_json::Value, pub enabled: bool }`
  - `Store::list_sources_meta(&self) -> Result<Vec<SourceMeta>>`: reads WITHOUT touching or decrypting `secret_blob`.
  - `Store::update_source(&self, original_name: &str, s: &NewSource, keep_secrets: bool) -> Result<()>`: updates all non-secret fields (and the name itself); `keep_secrets: true` leaves the existing blob, `false` seals `s.secrets` fresh. Errors on unknown `original_name`.
  - `Store::recent_runs_view(&self, limit: i64) -> Result<Vec<RunView>>` with `store::RunView { pub source: String, pub kind: String, pub status: String, pub started_at: String, pub finished_at: Option<String>, pub detail: Option<String> }` (JOIN on sources for the name, newest first).
  - `types::Retention::parse_csv(s: &str) -> anyhow::Result<Retention>` (moved from main.rs's `parse_retention`; main delegates; behavior identical: "7,4,6").

- [ ] **Step 1: Write the failing tests**

`src/types.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_csv_roundtrip_and_rejects() {
        let r = Retention::parse_csv("7,4,6").unwrap();
        assert_eq!((r.daily, r.weekly, r.monthly), (7, 4, 6));
        assert!(Retention::parse_csv("7,4").is_err());
        assert!(Retention::parse_csv("a,b,c").is_err());
    }
}
```

`src/store.rs` additions to the tests module:

```rust
    #[test]
    fn verify_hc_uuid_roundtrips_and_migrates() {
        let st = store();
        let mut s = sample();
        s.verify_healthchecks_uuid = Some("vhc-123".into());
        st.add_source(&s).unwrap();
        assert_eq!(
            st.get_source("acme-db").unwrap().verify_healthchecks_uuid.as_deref(),
            Some("vhc-123")
        );
    }

    #[test]
    fn list_sources_meta_never_touches_secrets() {
        let st = store();
        st.add_source(&sample()).unwrap();
        let metas = st.list_sources_meta().unwrap();
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].name, "acme-db");
        assert_eq!(metas[0].retention.daily, 7);
        // SourceMeta has no secrets field at all: enforced by the type.
    }

    #[test]
    fn update_source_keep_secrets_preserves_blob() {
        let st = store();
        st.add_source(&sample()).unwrap();
        let mut edited = sample();
        edited.schedule = "0 3 * * *".into();
        edited.secrets = std::collections::HashMap::new();
        st.update_source("acme-db", &edited, true).unwrap();
        let row = st.get_source("acme-db").unwrap();
        assert_eq!(row.schedule, "0 3 * * *");
        assert_eq!(row.secrets.get("password").unwrap(), "pw", "blob preserved");
    }

    #[test]
    fn update_source_reseal_replaces_secrets_and_can_rename() {
        let st = store();
        st.add_source(&sample()).unwrap();
        let mut edited = sample();
        edited.name = "acme-db2".into();
        edited.secrets = std::collections::HashMap::from([("password".to_string(), "pw2".to_string())]);
        st.update_source("acme-db", &edited, false).unwrap();
        assert!(st.get_source("acme-db").is_err());
        assert_eq!(st.get_source("acme-db2").unwrap().secrets.get("password").unwrap(), "pw2");
    }

    #[test]
    fn recent_runs_view_joins_source_names() {
        let st = store();
        let sid = st.add_source(&sample()).unwrap();
        let r = st.start_run(sid, "backup").unwrap();
        st.finish_run(r, "success", None, Some("snapX"), None).unwrap();
        let views = st.recent_runs_view(10).unwrap();
        assert_eq!(views[0].source, "acme-db");
        assert_eq!(views[0].kind, "backup");
        assert_eq!(views[0].status, "success");
    }
```

`tests/cli.rs`: extend the verify-schedule CLI test to also pass `--verify-healthchecks-uuid hc-test-uuid` on the valid add and assert success (name the test suffix appropriately).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test`
Expected: compile errors (new fields/methods/types missing).

- [ ] **Step 3: Implement**

`src/types.rs`:

```rust
impl Retention {
    /// Parses "daily,weekly,monthly", e.g. "7,4,6".
    pub fn parse_csv(s: &str) -> anyhow::Result<Retention> {
        use anyhow::Context;
        let parts: Vec<u32> = s
            .split(',')
            .map(|p| p.trim().parse::<u32>().context("retention must be daily,weekly,monthly numbers"))
            .collect::<anyhow::Result<_>>()?;
        anyhow::ensure!(parts.len() == 3, "retention must have exactly three numbers: daily,weekly,monthly");
        Ok(Retention { daily: parts[0], weekly: parts[1], monthly: parts[2] })
    }
}
```

`src/store.rs`: after `execute_batch(MIGRATIONS)`, add the pragma-guarded column migration:

```rust
        let has_verify_hc: bool = conn
            .prepare("PRAGMA table_info(sources)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .any(|c| c == "verify_healthchecks_uuid");
        if !has_verify_hc {
            conn.execute_batch("ALTER TABLE sources ADD COLUMN verify_healthchecks_uuid TEXT;")?;
        }
```

Add the field to `NewSource`/`SourceRow`, thread it through `add_source` (INSERT column list), `row_to_source`, and the MIGRATIONS `CREATE TABLE` text (new installs get it natively). Implement `SourceMeta`, `list_sources_meta` (explicit column list, no `secret_blob`), `update_source` (UPDATE with or without `secret_blob = ?`), `RunView`, `recent_runs_view`:

```rust
    pub fn list_sources_meta(&self) -> Result<Vec<SourceMeta>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, engine, schedule, verify_schedule, retention_json,
                    healthchecks_uuid, verify_healthchecks_uuid, settings_json, enabled
             FROM sources ORDER BY name",
        )?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            out.push(SourceMeta {
                id: r.get(0)?,
                name: r.get(1)?,
                engine: r.get(2)?,
                schedule: r.get(3)?,
                verify_schedule: r.get(4)?,
                retention: serde_json::from_str(&r.get::<_, String>(5)?)?,
                healthchecks_uuid: r.get(6)?,
                verify_healthchecks_uuid: r.get(7)?,
                settings: serde_json::from_str(&r.get::<_, String>(8)?)?,
                enabled: r.get::<_, i64>(9)? != 0,
            });
        }
        Ok(out)
    }

    pub fn update_source(&self, original_name: &str, s: &NewSource, keep_secrets: bool) -> Result<()> {
        crate::store::validate_name(&s.name)?;
        let n = if keep_secrets {
            self.conn.execute(
                "UPDATE sources SET name=?2, engine=?3, schedule=?4, verify_schedule=?5,
                 retention_json=?6, healthchecks_uuid=?7, verify_healthchecks_uuid=?8, settings_json=?9
                 WHERE name=?1",
                params![
                    original_name, s.name, s.engine, s.schedule, s.verify_schedule,
                    serde_json::to_string(&s.retention)?, s.healthchecks_uuid,
                    s.verify_healthchecks_uuid, serde_json::to_string(&s.settings)?
                ],
            )?
        } else {
            let blob = self.key.seal(serde_json::to_vec(&s.secrets)?.as_slice());
            self.conn.execute(
                "UPDATE sources SET name=?2, engine=?3, schedule=?4, verify_schedule=?5,
                 retention_json=?6, healthchecks_uuid=?7, verify_healthchecks_uuid=?8, settings_json=?9,
                 secret_blob=?10 WHERE name=?1",
                params![
                    original_name, s.name, s.engine, s.schedule, s.verify_schedule,
                    serde_json::to_string(&s.retention)?, s.healthchecks_uuid,
                    s.verify_healthchecks_uuid, serde_json::to_string(&s.settings)?, blob
                ],
            )?
        };
        anyhow::ensure!(n == 1, "no source named {original_name}");
        Ok(())
    }

    pub fn recent_runs_view(&self, limit: i64) -> Result<Vec<RunView>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.name, r.kind, r.status, r.started_at, r.finished_at, r.detail
             FROM runs r JOIN sources s ON s.id = r.source_id
             ORDER BY r.id DESC LIMIT ?1",
        )?;
        let mut rows = stmt.query(params![limit])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            out.push(RunView {
                source: r.get(0)?,
                kind: r.get(1)?,
                status: r.get(2)?,
                started_at: r.get(3)?,
                finished_at: r.get(4)?,
                detail: r.get(5)?,
            });
        }
        Ok(out)
    }
```

`src/exec.rs` `execute_verify`: the notify call passes `source.verify_healthchecks_uuid.as_deref()` with the rationale comment. `src/main.rs`: `--verify-healthchecks-uuid` flag threaded into `NewSource`; `parse_retention` deleted in favor of `types::Retention::parse_csv`.

- [ ] **Step 4: Run to verify pass**

Run: full gate.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: verify healthchecks separation, tui store plumbing"
```

---

### Task 2: Hardening leftovers (mandates + minors)

**Files:**
- Modify: `src/main.rs`, `src/scheduler.rs`
- Test: tests/cli.rs addition

**Interfaces:**
- Produces: check-config validates each source's `settings.timeout_minutes` (when present it must be an integer >= 1; anything else prints `<name>: timeout_minutes INVALID` and counts a problem); the `Snapshots` CLI arm builds its repo via `exec::build_repo(&cfg)` (removing the last inline duplicate); `VAULTKEEPER_RESTORE_TARGET` empty-string values are treated as unset (`.filter(|s| !s.is_empty())`); daemon boot logs at info the count of fresh (still-running) rows that survived reconciliation so a crash-restart is visible: `"{n} run row(s) still marked running (fresh, possibly in flight); they clear via the 24h bound if abandoned"` only when n > 0.

- [ ] **Step 1: Write the failing test**

`tests/cli.rs` new test `check_config_flags_bad_timeout_minutes`: temp config as in the existing check-config tests; add a source whose `--settings-json` is `{"host":"db.example.com","port":5432,"dbname":"app","user":"postgres","timeout_minutes":"soon"}` (string, invalid); run check-config; assert failure and stdout contains `timeout_minutes INVALID`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --test cli check_config_flags`
Expected: FAIL (no such validation).

- [ ] **Step 3: Implement**

check-config, inside the per-source loop:

```rust
                if let Some(tm) = s.settings.get("timeout_minutes") {
                    match tm.as_u64() {
                        Some(v) if v >= 1 => println!("{}: timeout_minutes ok ({v})", s.name),
                        _ => {
                            println!("{}: timeout_minutes INVALID (must be an integer >= 1)", s.name);
                            problems += 1;
                        }
                    }
                }
```

Snapshots arm: replace the inline ResticCli construction with `let repo = exec::build_repo(&cfg);`. Restore arm env fallback becomes `std::env::var("VAULTKEEPER_RESTORE_TARGET").ok().filter(|s| !s.is_empty())`. Scheduler boot, after `reconcile_stale_running`:

```rust
    let fresh_running: i64 = 0; // replaced by a store call below
```

Add `Store::count_running(&self) -> Result<u64>` (`SELECT count(*) FROM runs WHERE status = 'running'`, one-line, tested with a quick store unit test alongside) and log per the Produces text.

- [ ] **Step 4: Run to verify pass**

Run: full gate.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: timeout validation, repo helper reuse, boot visibility"
```

---

### Task 3: TUI foundation: state core, text input, dashboard + history render

**Files:**
- Create: `src/tui/mod.rs`, `src/tui/state.rs`, `src/tui/input.rs`, `src/tui/ui.rs`, `src/tui/data.rs`
- Modify: `Cargo.toml` (add `ratatui = "0.29"`), `src/main.rs` (`mod tui;` + `Tui` subcommand)
- Test: inline in state.rs, input.rs, ui.rs

**Interfaces:**
- Consumes: `store::{SourceMeta, RunView}`, `schedule::next_occurrence`.
- Produces (Tasks 4-6 implement against these EXACT types; define them all now):

```rust
// state.rs
pub enum Tab { Dashboard, History, Sources, Snapshots }
pub enum ActionKind { Backup, Verify }
pub enum Mode {
    Browse,
    SourceForm(Box<crate::tui::state::SourceForm>),
    RestoreTarget { snapshot_id: String, field: crate::tui::input::TextField },
    ConfirmRestore { snapshot_id: String, target: Option<String>, typed: crate::tui::input::TextField },
    Help,
}
pub enum Command {
    Refresh,
    Quit,
    LoadSnapshots(String),
    RunBackup(String),
    RunVerify(String),
    Restore { source: String, snapshot: String, target: Option<String> },
    SaveSource { draft: crate::store::NewSource, editing: Option<String>, keep_secrets: bool },
    SetEnabled(String, bool),
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
impl App {
    pub fn new() -> App;
    pub fn selected_source(&self) -> Option<&crate::store::SourceMeta>;
    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> Option<Command>;
}
pub fn status_color(status: &str) -> ratatui::style::Color; // success green, verify_passed cyan, success_prune_failed yellow, failed/verify_failed red, everything else gray
```

```rust
// input.rs
pub struct TextField { pub masked: bool, /* private buffer */ }
impl TextField {
    pub fn new(masked: bool) -> TextField;
    pub fn handle(&mut self, key: crossterm::event::KeyEvent); // chars, backspace
    pub fn value(&self) -> &str;
    pub fn display(&self) -> String; // value or '*' repeated when masked
    pub fn set(&mut self, v: &str);
}
```

```rust
// data.rs (Task 3 implements ONLY refresh; workers arrive in Task 4)
pub enum Event {
    Refreshed { sources: Vec<crate::store::SourceMeta>, runs: Vec<crate::store::RunView> },
    Snapshots { source: String, snapshots: Vec<crate::restic::Snapshot> },
    ActionDone(String),
    ActionFailed(String),
}
pub struct DataHub { /* cfg: Arc<config::Config>, db_path: String, tx/rx: mpsc */ }
impl DataHub {
    pub fn new(cfg: crate::config::Config, db_path: String) -> anyhow::Result<DataHub>;
    pub fn refresh(&self) -> anyhow::Result<Event>; // synchronous SQLite read
    pub fn try_recv(&self) -> Option<Event>;
}
```

```rust
// ui.rs
pub fn render(f: &mut ratatui::Frame, app: &App); // tabs bar, tab body, status line
```

```rust
// mod.rs
pub fn run(cfg: crate::config::Config, db_path: String) -> anyhow::Result<()>;
// errors "the tui needs an interactive terminal" when stdout is not a TTY (std::io::IsTerminal)
```

Browse-mode keys this task implements in `handle_key`: `q` -> Quit; Tab/BackTab cycle tabs; Up/Down move the active list selection (per tab); `r` on Dashboard/Sources -> `RunBackup(selected)`; `v` -> `RunVerify(selected)`; `?` toggles Help; Enter on Snapshots tab and form keys are wired in Tasks 4-6 (return None for now with the match arms present).

- [ ] **Step 1: Write the failing tests**

`src/tui/input.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent};

    #[test]
    fn typing_and_backspace() {
        let mut f = TextField::new(false);
        f.handle(KeyEvent::from(KeyCode::Char('a')));
        f.handle(KeyEvent::from(KeyCode::Char('b')));
        f.handle(KeyEvent::from(KeyCode::Backspace));
        assert_eq!(f.value(), "a");
    }

    #[test]
    fn masked_display_hides_content() {
        let mut f = TextField::new(true);
        f.set("hunter2");
        assert_eq!(f.display(), "*******");
        assert_eq!(f.value(), "hunter2");
    }
}
```

`src/tui/state.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent};

    fn meta(name: &str) -> crate::store::SourceMeta {
        crate::store::SourceMeta {
            id: 1,
            name: name.into(),
            engine: "postgres".into(),
            schedule: "0 2 * * *".into(),
            verify_schedule: None,
            retention: crate::types::Retention { daily: 7, weekly: 4, monthly: 6 },
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
        assert!(matches!(app.handle_key(KeyEvent::from(KeyCode::Char('q'))), Some(Command::Quit)));
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
}
```

(`Command` needs `#[derive(Debug)]`: it carries no secrets, only names/ids; the restore target is Option<String> and MAY carry a password-bearing URL: implement Debug MANUALLY for Command redacting the target field to "<target set>"; add a test asserting the debug output of a Restore command does not contain the target string.)

```rust
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
```

`src/tui/ui.rs` TestBackend smoke:

```rust
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
}
```

(Make the state tests' `meta` helper `pub(crate)` inside `#[cfg(test)]` so ui tests reuse it, or duplicate the fixture; either is fine, disclose the choice.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test tui`
Expected: compile errors, modules missing.

- [ ] **Step 3: Implement**

`input.rs` (complete):

```rust
use crossterm::event::{KeyCode, KeyEvent};

pub struct TextField {
    pub masked: bool,
    buffer: String,
}

impl TextField {
    pub fn new(masked: bool) -> TextField {
        TextField { masked, buffer: String::new() }
    }

    pub fn handle(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char(c) => self.buffer.push(c),
            KeyCode::Backspace => {
                self.buffer.pop();
            }
            _ => {}
        }
    }

    pub fn value(&self) -> &str {
        &self.buffer
    }

    pub fn display(&self) -> String {
        if self.masked {
            "*".repeat(self.buffer.chars().count())
        } else {
            self.buffer.clone()
        }
    }

    pub fn set(&mut self, v: &str) {
        self.buffer = v.to_string();
    }
}
```

`state.rs`: the types exactly as declared in Interfaces. `App::handle_key` in Browse mode: a match on `(self.tab, key.code)` implementing tab cycling (Tab/BackTab wrap through the four tabs), Up/Down clamped selection per tab (`sel_source` on Dashboard/Sources, `sel_run` on History, `sel_snapshot` on Snapshots), `q` Quit, `?` toggling `Mode::Help` and back, `r`/`v` producing RunBackup/RunVerify of `selected_source()` when the tab is Dashboard or Sources and a source exists. Non-Browse modes return None in this task (their arms exist and delegate to functions Tasks 5-6 fill). `status_color` per the test. Manual `Debug` for `Command` redacting the restore target.

`data.rs`: `DataHub::new` opens nothing eagerly (stores cfg in `Arc`, db_path, creates the mpsc pair); `refresh` opens a Store (`MasterKey::from_env`), reads `list_sources_meta` + `recent_runs_view(200)`, returns `Event::Refreshed`; `try_recv` drains the channel non-blockingly. Workers arrive in Task 4 (leave the tx clone plumbing in place).

`ui.rs`: `render` draws a 3-chunk vertical layout (tabs bar via `ratatui::widgets::Tabs`, body, status line paragraph). Dashboard body: `Table` with columns Name, Engine, Schedule, Enabled, Last status (styled with `status_color`), Last run, Next run (via `schedule::next_occurrence(schedule, Local::now())` formatted, "?" on error); the last-status columns come from the newest matching entry in `app.runs`. History body: `Table` of runs (source, kind, status styled, started, finished) plus a bottom detail `Paragraph` showing the selected run's detail (truncated by the widget). Sources/Snapshots bodies render placeholder tables this task (data exists for sources; snapshots fill in Task 4); `Mode::Help` draws a centered overlay listing the keys. Selected rows use `ratatui::widgets::TableState` derived from the sel_* fields.

`mod.rs`:

```rust
pub mod data;
pub mod input;
pub mod state;
pub mod ui;

use anyhow::{Context, Result};
use crossterm::event::{self, Event as CtEvent};
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
        terminal.draw(|f| ui::render(f, app)).context("draw failed")?;
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
                if key.kind != crossterm::event::KeyEventKind::Press {
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

fn dispatch(app: &mut state::App, hub: &data::DataHub, cmd: state::Command) {
    // Task 4 wires LoadSnapshots/RunBackup/RunVerify, Task 5 SaveSource/SetEnabled, Task 6 Restore.
    let _ = (app, hub, cmd);
}
```

`main.rs`: `Tui` subcommand loading config and calling `tui::run(cfg, db_path())`. Note the crossterm dependency comes via ratatui's re-export: import as `ratatui::crossterm::event` if the resolved ratatui exposes it; otherwise add `crossterm` matching ratatui's version to Cargo.toml (pre-authorized adaptation, disclose which).

- [ ] **Step 4: Run to verify pass**

Run: full gate. Also run a manual smoke on this machine: `cargo run -- tui` in a real terminal with a temp VAULTKEEPER_DB (it should render the empty dashboard and quit on q); capture what you observed in the report.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: tui foundation with dashboard and history"
```

---

### Task 4: TUI workers: run/verify actions and snapshot browsing

**Files:**
- Modify: `src/tui/data.rs`, `src/tui/mod.rs`, `src/tui/state.rs`, `src/tui/ui.rs`
- Test: inline in state.rs (event application), data.rs (labels)

**Interfaces:**
- Consumes: `exec::{execute_source, execute_verify}`, `exec::build_repo`, `restic::{Repo as _, latest via snapshots}`.
- Produces:
  - `DataHub::spawn_backup(&self, name: String)`, `spawn_verify(&self, name: String)`, `load_snapshots(&self, name: String)`: each `std::thread::spawn`s, clones an `mpsc::Sender`, runs the blocking work, sends `ActionDone("backup acme-db")`/`ActionFailed("backup acme-db: <err>")` or `Event::Snapshots{..}` (snapshots sorted newest first by parsed RFC3339 time, reusing the comparator logic from `restic::latest_snapshot` by sorting then reversing).
  - `mod.rs::dispatch` wires `LoadSnapshots`/`RunBackup`/`RunVerify` (pushing a label onto `app.busy` and setting the status line), Enter on the Snapshots tab triggers `LoadSnapshots(selected)` when `snapshots_for != Some(selected)`.
  - ui.rs: Snapshots tab renders id (short), time, tags for `app.snapshots` with the selection; the status line shows `busy` labels; Dashboard rows whose latest run status is `running` render gray "running".

- [ ] **Step 1: Write the failing tests**

state.rs:

```rust
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
```

data.rs (pure label helper so threads and tests share it):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_labels() {
        assert_eq!(action_label("backup", "a-db"), "backup a-db");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test tui`
Expected: compile errors.

- [ ] **Step 3: Implement**

data.rs workers (backup shown; verify is identical with `execute_verify`):

```rust
pub fn action_label(kind: &str, name: &str) -> String {
    format!("{kind} {name}")
}

impl DataHub {
    pub fn spawn_backup(&self, name: String) {
        let tx = self.tx.clone();
        let cfg = self.cfg.clone();
        let db = self.db_path.clone();
        std::thread::spawn(move || {
            let label = action_label("backup", &name);
            let res = crate::exec::execute_source(&cfg, &db, &name);
            let _ = match res {
                Ok(out) => tx.send(Event::ActionDone(format!("{label}: {}", out.status))),
                Err(e) => tx.send(Event::ActionFailed(format!("{label}: {e:#}"))),
            };
        });
    }

    pub fn load_snapshots(&self, name: String) {
        let tx = self.tx.clone();
        let cfg = self.cfg.clone();
        std::thread::spawn(move || {
            use crate::restic::Repo as _;
            let repo = crate::exec::build_repo(&cfg);
            let tag = format!("source={name}");
            match repo.snapshots(Some(&tag)) {
                Ok(mut snaps) => {
                    snaps.sort_by_key(|s| {
                        chrono::DateTime::parse_from_rfc3339(&s.time)
                            .map(|t| t.timestamp())
                            .unwrap_or(i64::MIN)
                    });
                    snaps.reverse();
                    let _ = tx.send(Event::Snapshots { source: name, snapshots: snaps });
                }
                Err(e) => {
                    let _ = tx.send(Event::ActionFailed(format!("snapshots {name}: {e:#}")));
                }
            }
        });
    }
}
```

(`exec::build_repo` may need its visibility widened from `pub(crate)` to stay callable here: it already is crate-visible, fine. `DataHub.cfg` becomes `Arc<crate::config::Config>`; `Config` needs no Clone since the Arc is created once in `new`.)

mod.rs `dispatch` fills in the three arms (busy push + status line + hub call). state.rs adds the Enter arm on Snapshots. ui.rs renders the snapshots table and busy list.

- [ ] **Step 4: Run to verify pass**

Run: full gate.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: tui run and verify actions, snapshot browsing"
```

---

### Task 5: TUI source management (add, edit, disable) with masked credentials

**Files:**
- Modify: `src/tui/state.rs`, `src/tui/ui.rs`, `src/tui/mod.rs`
- Test: inline in state.rs

**Interfaces:**
- Consumes: `store::{NewSource, SourceMeta}`, `types::Retention::parse_csv`, `schedule::validate`, `engines::engine_for`, and (via mod.rs dispatch) `Store::{add_source, update_source, set_enabled}`.
- Produces:
  - `state::SourceForm { pub editing: Option<String>, pub focus: usize, pub fields: Vec<(String, crate::tui::input::TextField)> }` with fields in EXACTLY this order: name, engine, schedule, verify_schedule, retention, healthchecks_uuid, verify_healthchecks_uuid, settings_json, secrets_json (the last masked). `SourceForm::new_add() -> SourceForm`; `SourceForm::new_edit(meta: &SourceMeta) -> SourceForm` (pre-fills everything EXCEPT secrets, which stays empty; retention rendered as csv; settings as compact JSON).
  - `SourceForm::validate(&self) -> anyhow::Result<(crate::store::NewSource, bool)>`: the bool is `keep_secrets` (true iff editing and the secrets field is empty). Validation: name via store rules (delegate by attempting `crate::store::validate_name`), engine via `engine_for`, schedule + optional verify_schedule via `schedule::validate`, retention via `parse_csv`, settings/secrets via serde_json (empty settings -> `{}`; empty secrets on ADD -> empty map).
  - Keys: on the Sources tab in Browse mode: `a` opens `Mode::SourceForm(new_add)`, `e` opens edit for the selection, `d` produces `Command::SetEnabled(name, !enabled)`. Inside the form: Up/Down move focus, typing edits the focused field, Esc cancels to Browse, Enter validates and produces `Command::SaveSource { draft, editing, keep_secrets }` (on validation error: sets `app.status_line` to the error and stays in the form).
  - mod.rs dispatch: SaveSource opens a Store and calls add_source or update_source, then refresh; SetEnabled calls set_enabled then refresh; both report errors to the status line.
  - ui.rs renders the form as a centered overlay listing label + `field.display()` with the focused field highlighted; the secrets field ALWAYS renders via display() (masked).

- [ ] **Step 1: Write the failing tests**

```rust
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
        assert_eq!(form.fields[8].1.value(), "", "secrets never round-trip into the form");
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
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test tui`
Expected: compile errors (SourceForm missing).

- [ ] **Step 3: Implement**

SourceForm per the interface: `new_add` builds the nine named empty fields (secrets masked); `new_edit` pre-fills from the meta (retention via `format!("{},{},{}", ...)`, settings via `serde_json::to_string`); `validate` per the Produces contract building `NewSource` (empty optional fields -> None). App::handle_key gains the Sources-tab keys and a `Mode::SourceForm` arm routing keys to the form (focus movement, field.handle, Esc, Enter -> validate -> Command or status_line). mod.rs dispatch implements SaveSource/SetEnabled with a fresh `Store::open` per action (matching the CLI pattern) and refresh after. ui.rs renders the overlay via `ratatui::widgets::Clear` + bordered block.

- [ ] **Step 4: Run to verify pass**

Run: full gate.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: tui source management with masked credential forms"
```

---

### Task 6: TUI restore flow: snapshot pick, target entry, typed confirmation

**Files:**
- Modify: `src/tui/state.rs`, `src/tui/ui.rs`, `src/tui/mod.rs`, `src/tui/data.rs`
- Test: inline in state.rs

**Interfaces:**
- Consumes: `exec::execute_restore`.
- Produces:
  - On the Snapshots tab with snapshots loaded, `R` enters `Mode::RestoreTarget { snapshot_id: <selected>, field: TextField::new(true) }` (target text is masked: it may carry a password; the ui hint says: "target url, blank uses VAULTKEEPER_RESTORE_TARGET; input hidden").
  - Enter moves to `Mode::ConfirmRestore { snapshot_id, target (None when blank), typed: TextField::new(false) }`; Esc backs out one step.
  - In ConfirmRestore, Enter produces `Command::Restore { source, snapshot, target }` ONLY when `typed.value()` equals the source name exactly; otherwise it sets the status line to "type the source name exactly to confirm" and stays.
  - `DataHub::spawn_restore(&self, source: String, snapshot: String, target: Option<String>)`: worker thread calling `execute_restore(&cfg, &db, &source, Some(&snapshot), target.as_deref(), false, false)` (no force flags from the TUI; same-host and overwrite guards stay CLI-only by design, with that comment) reporting ActionDone/ActionFailed.
  - mod.rs dispatch wires Restore (busy label "restore <source>").

- [ ] **Step 1: Write the failing tests**

```rust
    fn snapshots_ready(app: &mut App) {
        app.sources = vec![meta("a-db")];
        app.tab = Tab::Snapshots;
        app.snapshots_for = Some("a-db".into());
        app.snapshots = vec![crate::restic::Snapshot {
            id: "deadbeef".into(),
            time: "2026-07-14T02:00:00Z".into(),
            tags: vec!["source=a-db".into()],
        }];
    }

    #[test]
    fn restore_flow_requires_exact_typed_name() {
        let mut app = App::new();
        snapshots_ready(&mut app);
        assert!(app.handle_key(KeyEvent::from(KeyCode::Char('R'))).is_none());
        assert!(matches!(app.mode, Mode::RestoreTarget { .. }));
        assert!(app.handle_key(KeyEvent::from(KeyCode::Enter)).is_none());
        assert!(matches!(app.mode, Mode::ConfirmRestore { .. }));
        for c in "wrong".chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
        assert!(app.handle_key(KeyEvent::from(KeyCode::Enter)).is_none(), "wrong name blocks");
        if let Mode::ConfirmRestore { typed, .. } = &mut app.mode {
            typed.set("a-db");
        } else {
            panic!("mode lost");
        }
        match app.handle_key(KeyEvent::from(KeyCode::Enter)) {
            Some(Command::Restore { source, snapshot, target }) => {
                assert_eq!(source, "a-db");
                assert_eq!(snapshot, "deadbeef");
                assert!(target.is_none());
            }
            other => panic!("expected Restore, got {other:?}"),
        }
        assert!(matches!(app.mode, Mode::Browse));
    }

    #[test]
    fn restore_target_field_is_masked() {
        let mut app = App::new();
        snapshots_ready(&mut app);
        app.handle_key(KeyEvent::from(KeyCode::Char('R')));
        if let Mode::RestoreTarget { field, .. } = &app.mode {
            assert!(field.masked, "target may carry a password");
        } else {
            panic!("wrong mode");
        }
    }

    #[test]
    fn esc_backs_out_of_restore_flow() {
        let mut app = App::new();
        snapshots_ready(&mut app);
        app.handle_key(KeyEvent::from(KeyCode::Char('R')));
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        app.handle_key(KeyEvent::from(KeyCode::Esc));
        assert!(matches!(app.mode, Mode::RestoreTarget { .. }));
        app.handle_key(KeyEvent::from(KeyCode::Esc));
        assert!(matches!(app.mode, Mode::Browse));
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test tui`
Expected: FAIL (R key unhandled).

- [ ] **Step 3: Implement**

state.rs mode arms per the contract (R only fires when `snapshots_for` matches the selected source and a snapshot is selected); data.rs `spawn_restore` mirroring the other workers with the guards-stay-CLI comment; mod.rs dispatch arm; ui.rs renders both overlays (RestoreTarget shows the hint text and `field.display()`; ConfirmRestore shows snapshot id, "target: <set|env>", and the typed field with the instruction line).

- [ ] **Step 4: Run to verify pass**

Run: full gate. Manual smoke: with a temp db and a fake source, walk R -> Enter -> type name in a real terminal; capture observations in the report (no restic needed: the restore will fail in the worker and surface via the status line, which is itself the ActionFailed path working).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: tui restore with snapshot pick and typed confirmation"
```

---

### Task 7: Image and smoke minors

**Files:**
- Modify: `Dockerfile`, `scripts/smoke.sh`
- Test: CI (disclosed)

**Interfaces:**
- Produces: mongo repo list fetched over https (`https://repo.mongodb.org/apt/debian`); `LABEL org.opencontainers.image.source="https://github.com/salq791/vaultkeeper"` in the runtime stage (makes the GHCR package link to the repo and inherit its visibility on future publishes); curl purged alongside gnupg after the supabase deb install (`apt-get purge -y gnupg curl`); smoke.sh's cleanup trap dumps diagnostics on failure: the trap becomes an EXIT handler that checks `$?` and runs `$COMPOSE logs --tail 100` before `down -v` when nonzero.

- [ ] **Step 1: Apply the four edits**

Dockerfile: `http://repo.mongodb.org` -> `https://repo.mongodb.org`; add after `FROM debian:bookworm-slim` block's ARG: `LABEL org.opencontainers.image.source="https://github.com/salq791/vaultkeeper"`; `apt-get purge -y gnupg` -> `apt-get purge -y gnupg curl` (curl is only needed during build; verify nothing at runtime shells curl: grep src for "curl": the notify/hc path uses reqwest, so purging is safe; state the grep result in the report).

scripts/smoke.sh trap:

```bash
cleanup() {
  status=$?
  if [ "$status" -ne 0 ]; then
    echo "== smoke failed (exit $status): recent container logs =="
    $COMPOSE logs --tail 100 || true
  fi
  $COMPOSE down -v --remove-orphans >/dev/null 2>&1 || true
}
trap cleanup EXIT
```

- [ ] **Step 2: Validate and commit**

Run: `bash -n scripts/smoke.sh`, cargo gate (unchanged Rust).

```bash
git add Dockerfile scripts/smoke.sh
git commit -m "chore: image provenance label, https repo, curl purge, smoke diagnostics"
```

---

### Task 8: Launch docs

**Files:**
- Modify: `README.md`
- Create: `CHANGELOG.md`, `docs/announcement.md`

**Interfaces:**
- Produces: README with the roadmap replaced by a Features section (every line shipped, including the TUI), a Terminal UI section (how to open: `docker compose exec -it vaultkeeper vaultkeeper tui`; key reference table: Tab switch, arrows move, r run, v verify, a add, e edit, d enable/disable, Enter load snapshots, R restore, ? help, q quit; note that restores in the TUI honor the same guards and that credentials are entered masked and stored encrypted). CHANGELOG.md for v0.1.0 summarizing the six plans in user-facing terms. docs/announcement.md: a factual draft post for the Supabase community (what it backs up incl. the Storage/Edge Functions story, restic/BorgBase dedup, scheduled verify, what it does NOT do: Edge Function secrets, PITR on hosted Supabase, project settings; quickstart pointer; explicitly marked DRAFT for Sal's review at the top, since only Sal publishes outward-facing copy).

- [ ] **Step 1: Write the three documents**

README Features section (replace the Roadmap block):

```markdown
## Features

- Postgres (vanilla and Supabase), MongoDB, Supabase Storage, and Supabase Edge Functions backups on cron schedules
- Restic repositories (BorgBase or any restic backend): deduplication, encryption, retention pruning
- Restore with same-host guards and snapshot selection; storage restores require explicit overwrite confirmation
- Scheduled restore verification into scratch databases with row counts journaled
- healthchecks.io dead-man switch, webhook, and SES email alerting
- Hard timeouts on every child process; graceful SIGTERM shutdown; per-source concurrency guard
- Credentials encrypted at rest (ChaCha20-Poly1305, master key from env), entered via stdin or the masked TUI form, never argv
- Terminal UI for the whole loop: dashboard, history, sources, snapshots, restores
```

Terminal UI section (after Deploy), CHANGELOG.md, and docs/announcement.md per the Produces description; keep the announcement to ~30 lines, first line: `> DRAFT: for review before posting. Nothing below is published automatically.`

- [ ] **Step 2: Gate and commit**

```bash
git add -A
git commit -m "docs: features, terminal ui guide, changelog, launch draft"
```

---

## Self-Review Notes

- Spec coverage: every TUI element from the spec's TUI section maps to Tasks 3-6 (dashboard/history 3, run-verify-snapshots 4, sources + masked credentials + add/edit/disable 5, restore with restore-point selection + typed confirmation 6, docker exec usage in Task 8 docs); "never holds decrypted secrets in screen state" is structural (SourceMeta lacks the field; forms are write-only; restore target masked; Command Debug redacts). Carried mandates: verify hc UUID (Task 1), timeout validation + build_repo + boot visibility + empty-env filter (Task 2), image minors (Task 7). Launch (Task 8).
- Type consistency: Command/Mode/Event/SourceForm/SourceMeta/RunView declared once with exact signatures in their defining tasks and consumed by name elsewhere; field order of SourceForm pinned by tests.
- Placeholder scan: dispatch's Task-3 body explicitly defers named arms to named tasks with the wiring point in place; no TBDs.
- Consciously accepted: the interactive terminal loop and worker threads are untested glue (disclosed pattern); TUI e2e is manual smokes captured in reports; ratatui version may resolve crossterm re-export differently (pre-authorized adaptation).
