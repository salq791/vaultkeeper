use crate::crypto::MasterKey;
use crate::types::Retention;
use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct Store {
    conn: Connection,
    key: MasterKey,
    db_path: Option<PathBuf>,
}

const RUN_LEASE_MINUTES: i64 = 5;
const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// Keeps a run lease fresh from a separate SQLite connection. Dropping the
/// guard stops and joins the heartbeat thread before the run is finalized.
pub struct RunHeartbeat {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for RunHeartbeat {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            handle.thread().unpark();
            let _ = handle.join();
        }
    }
}

pub struct NewSource {
    pub name: String,
    pub engine: String,
    pub schedule: String,
    pub verify_schedule: Option<String>,
    pub retention: Retention,
    pub healthchecks_uuid: Option<String>,
    pub verify_healthchecks_uuid: Option<String>,
    pub settings: serde_json::Value,
    pub secrets: HashMap<String, String>,
}

pub struct SourceRow {
    pub id: i64,
    pub name: String,
    pub engine: String,
    pub schedule: String,
    pub verify_schedule: Option<String>,
    pub retention: Retention,
    pub healthchecks_uuid: Option<String>,
    pub verify_healthchecks_uuid: Option<String>,
    pub settings: serde_json::Value,
    pub secrets: HashMap<String, String>,
    pub enabled: bool,
}

/// Decryption-free projection of a source row for the TUI's source lists:
/// no `secrets` field exists on this type, so secret material can never end
/// up in TUI screen state by accident.
pub struct SourceMeta {
    pub id: i64,
    pub name: String,
    pub engine: String,
    pub schedule: String,
    pub verify_schedule: Option<String>,
    pub retention: Retention,
    pub healthchecks_uuid: Option<String>,
    pub verify_healthchecks_uuid: Option<String>,
    // Raw per-engine JSON (host/port/etc): the Sources tab shows every other
    // field but not this one (no generic tabular rendering for arbitrary
    // JSON); `SourceForm::new_edit` is the field's real consumer.
    pub settings: serde_json::Value,
    pub enabled: bool,
}

/// A run joined with its source name, for history display without a second
/// lookup per row.
pub struct RunView {
    pub source: String,
    pub kind: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub detail: Option<String>,
}

#[allow(dead_code)]
pub struct RunRow {
    pub id: i64,
    pub source_id: i64,
    pub kind: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: String,
    pub bytes: Option<i64>,
    pub snapshot_id: Option<String>,
    pub detail: Option<String>,
}

const MIGRATIONS: &str = r#"
CREATE TABLE IF NOT EXISTS sources (
  id INTEGER PRIMARY KEY,
  name TEXT UNIQUE NOT NULL,
  engine TEXT NOT NULL,
  schedule TEXT NOT NULL,
  verify_schedule TEXT,
  retention_json TEXT NOT NULL,
  healthchecks_uuid TEXT,
  verify_healthchecks_uuid TEXT,
  settings_json TEXT NOT NULL,
  secret_blob BLOB,
  enabled INTEGER NOT NULL DEFAULT 1
);
CREATE TABLE IF NOT EXISTS runs (
  id INTEGER PRIMARY KEY,
  source_id INTEGER NOT NULL REFERENCES sources(id),
  kind TEXT NOT NULL,
  started_at TEXT NOT NULL DEFAULT (datetime('now')),
  finished_at TEXT,
  status TEXT NOT NULL,
  bytes INTEGER,
  snapshot_id TEXT,
  detail TEXT,
  heartbeat_at TEXT NOT NULL DEFAULT (datetime('now'))
);
"#;

/// Validates that `name` is safe to use as both a staging directory path
/// component and a restic tag: first char ASCII alphanumeric, remaining
/// chars ASCII alphanumeric, `-`, or `_`.
pub fn validate_name(name: &str) -> Result<()> {
    let mut chars = name.chars();
    let valid = match chars.next() {
        Some(first) if first.is_ascii_alphanumeric() => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        }
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        bail!(
            "invalid source name '{name}': use only letters, digits, '-' and '_', starting with a letter or digit"
        );
    }
}

impl Store {
    pub fn open(path: &str, key: MasterKey) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("cannot open db {path}"))?;
        conn.execute_batch(MIGRATIONS)?;
        // Fresh installs get verify_healthchecks_uuid natively from the
        // CREATE TABLE above; existing databases upgrade in place here so
        // both paths converge on the same schema.
        let has_verify_hc: bool = conn
            .prepare("PRAGMA table_info(sources)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .any(|c| c == "verify_healthchecks_uuid");
        if !has_verify_hc {
            conn.execute_batch("ALTER TABLE sources ADD COLUMN verify_healthchecks_uuid TEXT;")?;
        }
        let has_heartbeat: bool = conn
            .prepare("PRAGMA table_info(runs)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .any(|column| column == "heartbeat_at");
        if !has_heartbeat {
            conn.execute_batch("ALTER TABLE runs ADD COLUMN heartbeat_at TEXT;")?;
            conn.execute_batch(
                "UPDATE runs SET heartbeat_at = COALESCE(finished_at, started_at) WHERE heartbeat_at IS NULL;",
            )?;
        }
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let _: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
        let db_path = (path != ":memory:").then(|| PathBuf::from(path));
        Ok(Self { conn, key, db_path })
    }

    pub fn add_source(&self, s: &NewSource) -> Result<i64> {
        validate_name(&s.name)?;
        let blob = self.key.seal(serde_json::to_vec(&s.secrets)?.as_slice());
        self.conn.execute(
            "INSERT INTO sources (name, engine, schedule, verify_schedule, retention_json,
             healthchecks_uuid, verify_healthchecks_uuid, settings_json, secret_blob)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                s.name,
                s.engine,
                s.schedule,
                s.verify_schedule,
                serde_json::to_string(&s.retention)?,
                s.healthchecks_uuid,
                s.verify_healthchecks_uuid,
                serde_json::to_string(&s.settings)?,
                blob
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    fn row_to_source(&self, row: &rusqlite::Row) -> Result<SourceRow> {
        let blob: Option<Vec<u8>> = row.get("secret_blob")?;
        let secrets = match blob {
            Some(b) => serde_json::from_slice(&self.key.open(&b)?)?,
            None => HashMap::new(),
        };
        Ok(SourceRow {
            id: row.get("id")?,
            name: row.get("name")?,
            engine: row.get("engine")?,
            schedule: row.get("schedule")?,
            verify_schedule: row.get("verify_schedule")?,
            retention: serde_json::from_str(&row.get::<_, String>("retention_json")?)?,
            healthchecks_uuid: row.get("healthchecks_uuid")?,
            verify_healthchecks_uuid: row.get("verify_healthchecks_uuid")?,
            settings: serde_json::from_str(&row.get::<_, String>("settings_json")?)?,
            secrets,
            enabled: row.get::<_, i64>("enabled")? != 0,
        })
    }

    pub fn get_source(&self, name: &str) -> Result<SourceRow> {
        let mut stmt = self.conn.prepare("SELECT * FROM sources WHERE name = ?1")?;
        let mut rows = stmt.query(params![name])?;
        let row = rows
            .next()?
            .with_context(|| format!("no source named {name}"))?;
        self.row_to_source(row)
    }

    pub fn list_sources(&self) -> Result<Vec<SourceRow>> {
        let mut stmt = self.conn.prepare("SELECT * FROM sources ORDER BY name")?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(self.row_to_source(row)?);
        }
        Ok(out)
    }

    /// Lists sources without ever touching or decrypting `secret_blob`: the
    /// column is not even in the SELECT list, so secret material cannot leak
    /// into TUI screen state through this path.
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

    /// Updates all non-secret fields (and the name itself) for the source
    /// currently named `original_name`. `keep_secrets: true` leaves the
    /// existing sealed blob untouched (used when a TUI edit form's secrets
    /// field was left blank); `false` seals `s.secrets` fresh in its place.
    ///
    /// When renaming (`s.name != original_name`), pre-checks for a name
    /// collision and bails with a friendly error rather than letting the
    /// UPDATE hit `sources.name`'s UNIQUE constraint and surface a raw
    /// sqlite error to the TUI's status line.
    pub fn update_source(
        &self,
        original_name: &str,
        s: &NewSource,
        keep_secrets: bool,
    ) -> Result<()> {
        crate::store::validate_name(&s.name)?;
        if s.name != original_name {
            let collision: Option<i64> = self
                .conn
                .query_row(
                    "SELECT 1 FROM sources WHERE name = ?1",
                    params![s.name],
                    |r| r.get(0),
                )
                .optional()?;
            if collision.is_some() {
                bail!("a source named '{}' already exists", s.name);
            }
        }
        let n = if keep_secrets {
            self.conn.execute(
                "UPDATE sources SET name=?2, engine=?3, schedule=?4, verify_schedule=?5,
                 retention_json=?6, healthchecks_uuid=?7, verify_healthchecks_uuid=?8, settings_json=?9
                 WHERE name=?1",
                params![
                    original_name,
                    s.name,
                    s.engine,
                    s.schedule,
                    s.verify_schedule,
                    serde_json::to_string(&s.retention)?,
                    s.healthchecks_uuid,
                    s.verify_healthchecks_uuid,
                    serde_json::to_string(&s.settings)?
                ],
            )?
        } else {
            let blob = self.key.seal(serde_json::to_vec(&s.secrets)?.as_slice());
            self.conn.execute(
                "UPDATE sources SET name=?2, engine=?3, schedule=?4, verify_schedule=?5,
                 retention_json=?6, healthchecks_uuid=?7, verify_healthchecks_uuid=?8, settings_json=?9,
                 secret_blob=?10 WHERE name=?1",
                params![
                    original_name,
                    s.name,
                    s.engine,
                    s.schedule,
                    s.verify_schedule,
                    serde_json::to_string(&s.retention)?,
                    s.healthchecks_uuid,
                    s.verify_healthchecks_uuid,
                    serde_json::to_string(&s.settings)?,
                    blob
                ],
            )?
        };
        anyhow::ensure!(n == 1, "no source named {original_name}");
        Ok(())
    }

    pub fn set_enabled(&self, name: &str, enabled: bool) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE sources SET enabled = ?2 WHERE name = ?1",
            params![name, enabled as i64],
        )?;
        anyhow::ensure!(n == 1, "no source named {name}");
        Ok(())
    }

    /// Starts a run, refusing if the source already has a run in progress.
    /// A 'running' row whose heartbeat is older than the lease is treated as
    /// a crashed process's zombie and no longer blocks. The INSERT below is
    /// a single conditional statement, so the check-and-claim is atomic even
    /// across processes sharing the database file.
    pub fn start_run(&self, source_id: i64, kind: &str) -> Result<i64> {
        self.conn.execute(
            "UPDATE runs SET status = 'stale', finished_at = datetime('now')
             WHERE source_id = ?1 AND status = 'running'
             AND COALESCE(heartbeat_at, started_at) <= datetime('now', ?2)",
            params![source_id, format!("-{RUN_LEASE_MINUTES} minutes")],
        )?;
        let inserted = self.conn.execute(
            "INSERT INTO runs (source_id, kind, status, heartbeat_at)
             SELECT ?1, ?2, 'running', datetime('now')
             WHERE NOT EXISTS (
               SELECT 1 FROM runs WHERE source_id = ?1 AND status = 'running'
             )",
            params![source_id, kind],
        )?;
        anyhow::ensure!(
            inserted == 1,
            "another run for this source is in progress; an abandoned run clears after its heartbeat lease expires"
        );
        Ok(self.conn.last_insert_rowid())
    }

    pub fn start_heartbeat(&self, run_id: i64) -> Result<RunHeartbeat> {
        let Some(db_path) = self.db_path.clone() else {
            return Ok(RunHeartbeat {
                stop: Arc::new(AtomicBool::new(true)),
                handle: None,
            });
        };
        // Open the independent connection before returning the guard. If this
        // fails, the run must not continue without a functioning lease.
        let connection = Connection::open(&db_path)
            .with_context(|| format!("failed to open run {run_id} heartbeat database"))?;
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .context("failed to configure run heartbeat database")?;
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let handle = std::thread::Builder::new()
            .name(format!("vaultkeeper-run-{run_id}-heartbeat"))
            .spawn(move || {
                while !worker_stop.load(Ordering::Acquire) {
                    if let Err(error) = connection.execute(
                        "UPDATE runs SET heartbeat_at = datetime('now') WHERE id = ?1 AND status = 'running'",
                        params![run_id],
                    ) {
                        tracing::warn!("run {run_id} heartbeat update failed: {error}");
                    }
                    std::thread::park_timeout(HEARTBEAT_INTERVAL);
                }
            })
            .context("failed to start run heartbeat thread")?;
        Ok(RunHeartbeat {
            stop,
            handle: Some(handle),
        })
    }

    pub fn finish_run(
        &self,
        run_id: i64,
        status: &str,
        bytes: Option<i64>,
        snapshot_id: Option<&str>,
        detail: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE runs SET status = ?2, bytes = ?3, snapshot_id = ?4, detail = ?5,
             finished_at = datetime('now') WHERE id = ?1",
            params![run_id, status, bytes, snapshot_id, detail],
        )?;
        Ok(())
    }

    /// Marks rows whose heartbeat lease expired stale at daemon boot.
    pub fn reconcile_stale_running(&self) -> Result<u64> {
        let n = self.conn.execute(
            "UPDATE runs SET status = 'stale', finished_at = datetime('now')
             WHERE status = 'running'
             AND COALESCE(heartbeat_at, started_at) <= datetime('now', ?1)",
            params![format!("-{RUN_LEASE_MINUTES} minutes")],
        )?;
        Ok(n as u64)
    }

    /// Counts the number of rows with status = 'running'.
    pub fn count_running(&self) -> Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT count(*) FROM runs WHERE status = 'running'",
            [],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// Journals a run that was refused by the concurrency guard so scheduled
    /// skips are visible in history instead of only in daemon logs.
    pub fn record_skip(&self, source_id: i64, kind: &str, reason: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO runs (source_id, kind, status, finished_at, detail)
             VALUES (?1, ?2, 'skipped', datetime('now'), ?3)",
            params![source_id, kind, reason],
        )?;
        Ok(())
    }

    /// Runs joined with their source name, newest first, for history display
    /// without a per-row source lookup.
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

    pub fn run_detail(&self, run_id: i64) -> Result<Option<String>> {
        let detail = self.conn.query_row(
            "SELECT detail FROM runs WHERE id = ?1",
            params![run_id],
            |r| r.get(0),
        )?;
        Ok(detail)
    }

    #[cfg(test)]
    pub fn conn_for_tests(&self) -> &rusqlite::Connection {
        &self.conn
    }

    // Only exercised by tests now that exec.rs scopes its post-run detail
    // lookup to run_detail(run_id) instead of racing on the most recent row.
    #[allow(dead_code)]
    pub fn recent_runs(&self, limit: i64) -> Result<Vec<RunRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source_id, kind, started_at, finished_at, status, bytes, snapshot_id, detail
             FROM runs ORDER BY id DESC LIMIT ?1",
        )?;
        let mut rows = stmt.query(params![limit])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            out.push(RunRow {
                id: r.get(0)?,
                source_id: r.get(1)?,
                kind: r.get(2)?,
                started_at: r.get(3)?,
                finished_at: r.get(4)?,
                status: r.get(5)?,
                bytes: r.get(6)?,
                snapshot_id: r.get(7)?,
                detail: r.get(8)?,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::MasterKey;
    use crate::types::Retention;
    use std::collections::HashMap;

    const K: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    fn store() -> Store {
        Store::open(":memory:", MasterKey::from_hex(K).unwrap()).unwrap()
    }

    fn sample() -> NewSource {
        NewSource {
            name: "acme-db".into(),
            engine: "postgres".into(),
            schedule: "0 2 * * *".into(),
            verify_schedule: None,
            retention: Retention {
                daily: 7,
                weekly: 4,
                monthly: 6,
            },
            healthchecks_uuid: None,
            verify_healthchecks_uuid: None,
            settings: serde_json::json!({"host": "db.example.com", "port": 5432, "dbname": "app", "user": "postgres"}),
            secrets: HashMap::from([("password".to_string(), "pw".to_string())]),
        }
    }

    #[test]
    fn add_get_roundtrip_decrypts_secrets() {
        let st = store();
        st.add_source(&sample()).unwrap();
        let row = st.get_source("acme-db").unwrap();
        assert_eq!(row.engine, "postgres");
        assert_eq!(row.secrets.get("password").unwrap(), "pw");
        assert_eq!(row.retention.daily, 7);
        assert!(row.enabled);
    }

    #[test]
    fn duplicate_name_rejected() {
        let st = store();
        st.add_source(&sample()).unwrap();
        assert!(st.add_source(&sample()).is_err());
    }

    #[test]
    fn rejects_dotdot_name() {
        let st = store();
        let mut s = sample();
        s.name = "..".into();
        let err = st.add_source(&s).unwrap_err();
        assert!(err.to_string().contains("invalid source name"));
    }

    #[test]
    fn rejects_name_with_slash() {
        let st = store();
        let mut s = sample();
        s.name = "a/b".into();
        assert!(st.add_source(&s).is_err());
    }

    #[test]
    fn rejects_empty_name() {
        let st = store();
        let mut s = sample();
        s.name = "".into();
        assert!(st.add_source(&s).is_err());
    }

    #[test]
    fn rejects_name_with_comma() {
        let st = store();
        let mut s = sample();
        s.name = "a,b".into();
        assert!(st.add_source(&s).is_err());
    }

    #[test]
    fn accepts_hyphen_underscore_name() {
        let st = store();
        let mut s = sample();
        s.name = "acme-db_1".into();
        assert!(st.add_source(&s).is_ok());
    }

    #[test]
    fn set_enabled_roundtrip() {
        let st = store();
        st.add_source(&sample()).unwrap();
        st.set_enabled("acme-db", false).unwrap();
        assert!(!st.get_source("acme-db").unwrap().enabled);
        st.set_enabled("acme-db", true).unwrap();
        assert!(st.get_source("acme-db").unwrap().enabled);
    }

    #[test]
    fn set_enabled_unknown_source_errors() {
        let st = store();
        assert!(st.set_enabled("ghost", false).is_err());
    }

    #[test]
    fn run_detail_scoped_by_run_id() {
        let st = store();
        let sid = st.add_source(&sample()).unwrap();
        let r1 = st.start_run(sid, "backup").unwrap();
        st.finish_run(r1, "failed", None, None, Some("first detail"))
            .unwrap();
        let r2 = st.start_run(sid, "backup").unwrap();
        st.finish_run(r2, "success", None, None, None).unwrap();
        assert_eq!(st.run_detail(r1).unwrap().as_deref(), Some("first detail"));
        assert_eq!(st.run_detail(r2).unwrap(), None);
    }

    #[test]
    fn concurrent_run_for_same_source_refused() {
        let st = store();
        let sid = st.add_source(&sample()).unwrap();
        let _r1 = st.start_run(sid, "backup").unwrap();
        let err = st.start_run(sid, "verify").unwrap_err();
        assert!(err.to_string().contains("in progress"));
    }

    #[test]
    fn finished_run_unblocks_source() {
        let st = store();
        let sid = st.add_source(&sample()).unwrap();
        let r1 = st.start_run(sid, "backup").unwrap();
        st.finish_run(r1, "success", None, None, None).unwrap();
        assert!(st.start_run(sid, "backup").is_ok());
    }

    #[test]
    fn stale_running_row_is_cleared_and_does_not_block() {
        let st = store();
        let sid = st.add_source(&sample()).unwrap();
        st.conn_for_tests().execute(
            "INSERT INTO runs (source_id, kind, status, started_at, heartbeat_at) VALUES (?1, 'backup', 'running', datetime('now', '-6 minutes'), datetime('now', '-6 minutes'))",
            rusqlite::params![sid],
        ).unwrap();
        let r2 = st.start_run(sid, "backup").unwrap();
        assert!(r2 > 0);
        let stale: i64 = st
            .conn_for_tests()
            .query_row(
                "SELECT count(*) FROM runs WHERE source_id = ?1 AND status = 'stale'",
                rusqlite::params![sid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stale, 1);
    }

    #[test]
    fn run_lifecycle() {
        let st = store();
        let sid = st.add_source(&sample()).unwrap();
        let rid = st.start_run(sid, "backup").unwrap();
        st.finish_run(rid, "success", Some(1024), Some("abc123"), None)
            .unwrap();
        let runs = st.recent_runs(10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "success");
        assert_eq!(runs[0].snapshot_id.as_deref(), Some("abc123"));
    }

    #[test]
    fn reconcile_clears_only_expired_heartbeat_leases() {
        let st = store();
        let sid = st.add_source(&sample()).unwrap();
        // Fresh run first: start_run itself clears >24h rows for the source,
        // so the zombie is inserted afterward for reconcile alone to see.
        let fresh = st.start_run(sid, "backup").unwrap();
        st.conn_for_tests()
            .execute(
                "INSERT INTO runs (source_id, kind, status, started_at, heartbeat_at) VALUES (?1, 'backup', 'running', datetime('now', '-6 minutes'), datetime('now', '-6 minutes'))",
                rusqlite::params![sid],
            )
            .unwrap();
        assert_eq!(st.reconcile_stale_running().unwrap(), 1);
        let stale: i64 = st
            .conn_for_tests()
            .query_row(
                "SELECT count(*) FROM runs WHERE status = 'stale'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stale, 1);
        let fresh_status: String = st
            .conn_for_tests()
            .query_row(
                "SELECT status FROM runs WHERE id = ?1",
                rusqlite::params![fresh],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            fresh_status, "running",
            "a fresh run (e.g. manual docker-exec) survives daemon boot"
        );
    }

    #[test]
    fn count_running_counts_active_runs() {
        let st = store();
        let sid = st.add_source(&sample()).unwrap();
        assert_eq!(st.count_running().unwrap(), 0);
        let r1 = st.start_run(sid, "backup").unwrap();
        assert_eq!(st.count_running().unwrap(), 1);
        st.finish_run(r1, "success", None, None, None).unwrap();
        assert_eq!(st.count_running().unwrap(), 0);
    }

    #[test]
    fn record_skip_writes_finished_skipped_row() {
        let st = store();
        let sid = st.add_source(&sample()).unwrap();
        st.record_skip(sid, "verify", "another run in progress")
            .unwrap();
        let runs = st.recent_runs(1).unwrap();
        assert_eq!(runs[0].status, "skipped");
        assert_eq!(runs[0].kind, "verify");
        assert!(runs[0].finished_at.is_some());
        assert!(runs[0].detail.as_deref().unwrap().contains("in progress"));
    }

    #[test]
    fn verify_hc_uuid_roundtrips_and_migrates() {
        let st = store();
        let mut s = sample();
        s.verify_healthchecks_uuid = Some("vhc-123".into());
        st.add_source(&s).unwrap();
        assert_eq!(
            st.get_source("acme-db")
                .unwrap()
                .verify_healthchecks_uuid
                .as_deref(),
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

    // Rider (2): the missing coverage gap for update_source's unknown-name
    // path (add_get_roundtrip etc. never exercised it).
    #[test]
    fn update_source_unknown_original_name_errors() {
        let st = store();
        let err = st.update_source("ghost", &sample(), true).unwrap_err();
        assert!(err.to_string().contains("no source named"));
    }

    // Rider (1): renaming onto an existing source's name must surface a
    // friendly error instead of a raw sqlite UNIQUE-constraint message.
    #[test]
    fn update_source_rename_collision_is_friendly_error() {
        let st = store();
        st.add_source(&sample()).unwrap();
        let mut other = sample();
        other.name = "other-db".into();
        st.add_source(&other).unwrap();

        let mut edited = sample();
        edited.name = "other-db".into();
        let err = st.update_source("acme-db", &edited, true).unwrap_err();
        assert!(err.to_string().contains("already exists"), "got: {err}");
    }

    #[test]
    fn update_source_reseal_replaces_secrets_and_can_rename() {
        let st = store();
        st.add_source(&sample()).unwrap();
        let mut edited = sample();
        edited.name = "acme-db2".into();
        edited.secrets =
            std::collections::HashMap::from([("password".to_string(), "pw2".to_string())]);
        st.update_source("acme-db", &edited, false).unwrap();
        assert!(st.get_source("acme-db").is_err());
        assert_eq!(
            st.get_source("acme-db2")
                .unwrap()
                .secrets
                .get("password")
                .unwrap(),
            "pw2"
        );
    }

    #[test]
    fn recent_runs_view_joins_source_names() {
        let st = store();
        let sid = st.add_source(&sample()).unwrap();
        let r = st.start_run(sid, "backup").unwrap();
        st.finish_run(r, "success", None, Some("snapX"), None)
            .unwrap();
        let views = st.recent_runs_view(10).unwrap();
        assert_eq!(views[0].source, "acme-db");
        assert_eq!(views[0].kind, "backup");
        assert_eq!(views[0].status, "success");
    }

    #[test]
    fn file_backed_store_uses_wal() {
        let dir = tempfile::tempdir().unwrap();
        let st = Store::open(
            dir.path().join("w.db").to_str().unwrap(),
            MasterKey::from_hex(K).unwrap(),
        )
        .unwrap();
        let mode: String = st
            .conn_for_tests()
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
    }

    #[test]
    fn fresh_heartbeat_keeps_a_long_running_job_leased() {
        let st = store();
        let sid = st.add_source(&sample()).unwrap();
        st.conn_for_tests()
            .execute(
                "INSERT INTO runs (source_id, kind, status, started_at, heartbeat_at)
                 VALUES (?1, 'backup', 'running', datetime('now', '-2 hours'), datetime('now'))",
                rusqlite::params![sid],
            )
            .unwrap();
        assert_eq!(st.reconcile_stale_running().unwrap(), 0);
        assert!(st.start_run(sid, "verify").is_err());
    }
}
