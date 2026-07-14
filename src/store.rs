use crate::crypto::MasterKey;
use crate::types::Retention;
use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection};
use std::collections::HashMap;

pub struct Store {
    conn: Connection,
    key: MasterKey,
}

pub struct NewSource {
    pub name: String,
    pub engine: String,
    pub schedule: String,
    pub verify_schedule: Option<String>,
    pub retention: Retention,
    pub healthchecks_uuid: Option<String>,
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
    pub settings: serde_json::Value,
    pub secrets: HashMap<String, String>,
    pub enabled: bool,
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
  detail TEXT
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
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let _: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
        Ok(Self { conn, key })
    }

    pub fn add_source(&self, s: &NewSource) -> Result<i64> {
        validate_name(&s.name)?;
        let blob = self.key.seal(serde_json::to_vec(&s.secrets)?.as_slice());
        self.conn.execute(
            "INSERT INTO sources (name, engine, schedule, verify_schedule, retention_json,
             healthchecks_uuid, settings_json, secret_blob)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                s.name,
                s.engine,
                s.schedule,
                s.verify_schedule,
                serde_json::to_string(&s.retention)?,
                s.healthchecks_uuid,
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

    pub fn set_enabled(&self, name: &str, enabled: bool) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE sources SET enabled = ?2 WHERE name = ?1",
            params![name, enabled as i64],
        )?;
        anyhow::ensure!(n == 1, "no source named {name}");
        Ok(())
    }

    /// Starts a run, refusing if the source already has a run in progress.
    /// A 'running' row older than 24 hours is treated as a crashed process's
    /// zombie: it is marked 'stale' and no longer blocks. The INSERT below is
    /// a single conditional statement, so the check-and-claim is atomic even
    /// across processes sharing the database file.
    pub fn start_run(&self, source_id: i64, kind: &str) -> Result<i64> {
        self.conn.execute(
            "UPDATE runs SET status = 'stale', finished_at = datetime('now')
             WHERE source_id = ?1 AND status = 'running'
             AND started_at <= datetime('now', '-24 hours')",
            params![source_id],
        )?;
        let inserted = self.conn.execute(
            "INSERT INTO runs (source_id, kind, status)
             SELECT ?1, ?2, 'running'
             WHERE NOT EXISTS (
               SELECT 1 FROM runs WHERE source_id = ?1 AND status = 'running'
             )",
            params![source_id, kind],
        )?;
        anyhow::ensure!(
            inserted == 1,
            "another run for this source is in progress; a run that crashed more than 24 hours ago clears automatically"
        );
        Ok(self.conn.last_insert_rowid())
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

    /// Marks 'running' rows older than 24 hours stale at daemon boot. Fresh
    /// running rows are left alone: a manual docker-exec run may legitimately
    /// be in flight across a daemon restart, and rows it abandons clear via
    /// the same 24h bound in start_run.
    pub fn reconcile_stale_running(&self) -> Result<u64> {
        let n = self.conn.execute(
            "UPDATE runs SET status = 'stale', finished_at = datetime('now')
             WHERE status = 'running' AND started_at <= datetime('now', '-24 hours')",
            [],
        )?;
        Ok(n as u64)
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
            "INSERT INTO runs (source_id, kind, status, started_at) VALUES (?1, 'backup', 'running', datetime('now', '-25 hours'))",
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
    fn reconcile_clears_only_old_zombie_rows() {
        let st = store();
        let sid = st.add_source(&sample()).unwrap();
        // Fresh run first: start_run itself clears >24h rows for the source,
        // so the zombie is inserted afterward for reconcile alone to see.
        let fresh = st.start_run(sid, "backup").unwrap();
        st.conn_for_tests()
            .execute(
                "INSERT INTO runs (source_id, kind, status, started_at) VALUES (?1, 'backup', 'running', datetime('now', '-25 hours'))",
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
}
