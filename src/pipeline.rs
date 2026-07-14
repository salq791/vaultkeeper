use crate::engines::{DumpCtx, Engine};
use crate::restic::Repo;
use crate::store::{SourceRow, Store};
use anyhow::Result;
use std::path::Path;

#[derive(Debug)]
pub struct RunOutcome {
    pub run_id: i64,
    pub snapshot_id: Option<String>,
    pub status: String,
}

pub fn run_backup(
    store: &Store,
    repo: &dyn Repo,
    source: &SourceRow,
    staging_root: &Path,
    engine: &dyn Engine,
) -> Result<RunOutcome> {
    crate::store::validate_name(&source.name)?;
    let run_id = store.start_run(source.id, "backup")?;
    let staging_dir = staging_root.join(&source.name);
    let result = (|| -> Result<(String, i64, Option<String>)> {
        repo.ensure_init()?;
        if staging_dir.exists() {
            std::fs::remove_dir_all(&staging_dir)?;
        }
        std::fs::create_dir_all(&staging_dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&staging_dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let mirror_root = staging_root.join(".mirrors").join(&source.name);
        std::fs::create_dir_all(&mirror_root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&mirror_root, std::fs::Permissions::from_mode(0o700))?;
        }
        let ctx = DumpCtx {
            staging_dir: staging_dir.clone(),
            mirror_root,
            settings: source.settings.clone(),
            secrets: source.secrets.clone(),
        };
        let backup_path = engine.dump(&ctx)?;
        let tag = format!("source={}", source.name);
        let summary = repo.backup(&backup_path, &tag)?;
        let prune_err = repo
            .forget(&tag, &source.retention)
            .err()
            .map(|e| crate::util::truncate_marked(&format!("{e:#}"), 4000));
        Ok((
            summary.snapshot_id,
            summary.total_bytes_processed,
            prune_err,
        ))
    })();
    let _ = std::fs::remove_dir_all(&staging_dir);

    match result {
        Ok((snapshot_id, bytes, None)) => {
            if let Err(journal_err) =
                store.finish_run(run_id, "success", Some(bytes), Some(&snapshot_id), None)
            {
                tracing::warn!("failed to journal run {run_id} success: {journal_err:#}");
            }
            Ok(RunOutcome {
                run_id,
                snapshot_id: Some(snapshot_id),
                status: "success".into(),
            })
        }
        Ok((snapshot_id, bytes, Some(prune_err))) => {
            if let Err(journal_err) = store.finish_run(
                run_id,
                "success_prune_failed",
                Some(bytes),
                Some(&snapshot_id),
                Some(&prune_err),
            ) {
                tracing::warn!(
                    "failed to journal run {run_id} success_prune_failed: {journal_err:#}"
                );
            }
            Ok(RunOutcome {
                run_id,
                snapshot_id: Some(snapshot_id),
                status: "success_prune_failed".into(),
            })
        }
        Err(e) => {
            let detail = crate::util::truncate_marked(&format!("{e:#}"), 4000);
            if let Err(journal_err) = store.finish_run(run_id, "failed", None, None, Some(&detail))
            {
                tracing::warn!("failed to journal run {run_id} failure: {journal_err:#}");
            }
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::MasterKey;
    use crate::engines::{DumpCtx, Engine, RestoreCtx, VerifyCtx};
    use crate::restic::{BackupSummary, Repo, Snapshot};
    use crate::store::{NewSource, Store};
    use crate::types::Retention;
    use anyhow::{bail, Result};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::path::Path;

    const K: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    struct OkEngine;
    impl Engine for OkEngine {
        fn dump(&self, ctx: &DumpCtx) -> Result<std::path::PathBuf> {
            std::fs::write(ctx.staging_dir.join("db.dump"), b"data")?;
            Ok(ctx.staging_dir.clone())
        }
        // Pipeline test double: dump-only, restore/verify are never exercised here.
        fn restore(&self, _ctx: &RestoreCtx) -> Result<()> {
            unreachable!()
        }
        fn verify(&self, _ctx: &VerifyCtx) -> Result<String> {
            unreachable!()
        }
    }

    struct FailEngine;
    impl Engine for FailEngine {
        fn dump(&self, _ctx: &DumpCtx) -> Result<std::path::PathBuf> {
            bail!("connection refused");
        }
        // Pipeline test double: dump-only, restore/verify are never exercised here.
        fn restore(&self, _ctx: &RestoreCtx) -> Result<()> {
            unreachable!()
        }
        fn verify(&self, _ctx: &VerifyCtx) -> Result<String> {
            unreachable!()
        }
    }

    struct MirrorEngine;
    impl Engine for MirrorEngine {
        fn dump(&self, ctx: &DumpCtx) -> Result<std::path::PathBuf> {
            std::fs::write(ctx.mirror_root.join("obj1"), b"filedata")?;
            Ok(ctx.mirror_root.clone())
        }
        // Pipeline test double: dump-only, restore/verify are never exercised here.
        fn restore(&self, _ctx: &RestoreCtx) -> Result<()> {
            unreachable!()
        }
        fn verify(&self, _ctx: &VerifyCtx) -> Result<String> {
            unreachable!()
        }
    }

    #[derive(Default)]
    struct MockRepo {
        calls: RefCell<Vec<String>>,
    }
    impl Repo for MockRepo {
        fn ensure_init(&self) -> Result<()> {
            Ok(())
        }
        fn backup(&self, _path: &Path, tag: &str) -> Result<BackupSummary> {
            self.calls.borrow_mut().push(format!(
                "backup:{tag}:{}",
                _path.file_name().and_then(|n| n.to_str()).unwrap_or("?")
            ));
            Ok(BackupSummary {
                snapshot_id: "snap1".into(),
                total_bytes_processed: 4,
            })
        }
        fn forget(&self, tag: &str, _r: &Retention) -> Result<()> {
            self.calls.borrow_mut().push(format!("forget:{tag}"));
            Ok(())
        }
        fn snapshots(&self, _tag: Option<&str>) -> Result<Vec<Snapshot>> {
            Ok(vec![])
        }
        fn restore(&self, _id: &str, _d: &Path) -> Result<()> {
            Ok(())
        }
    }

    struct PruneFailRepo;
    impl Repo for PruneFailRepo {
        fn ensure_init(&self) -> Result<()> {
            Ok(())
        }
        fn backup(&self, _path: &Path, _tag: &str) -> Result<BackupSummary> {
            Ok(BackupSummary {
                snapshot_id: "snap9".into(),
                total_bytes_processed: 7,
            })
        }
        fn forget(&self, _tag: &str, _r: &Retention) -> Result<()> {
            anyhow::bail!("repository is locked by another process")
        }
        fn snapshots(&self, _tag: Option<&str>) -> Result<Vec<Snapshot>> {
            Ok(vec![])
        }
        fn restore(&self, _id: &str, _d: &Path) -> Result<()> {
            Ok(())
        }
    }

    struct InitFailRepo;
    impl Repo for InitFailRepo {
        fn ensure_init(&self) -> Result<()> {
            anyhow::bail!("repository unreachable")
        }
        fn backup(&self, _p: &Path, _t: &str) -> Result<BackupSummary> {
            unreachable!()
        }
        fn forget(&self, _t: &str, _r: &Retention) -> Result<()> {
            unreachable!()
        }
        fn snapshots(&self, _t: Option<&str>) -> Result<Vec<Snapshot>> {
            Ok(vec![])
        }
        fn restore(&self, _id: &str, _d: &Path) -> Result<()> {
            Ok(())
        }
    }

    struct SabotageEngine {
        db_path: std::path::PathBuf,
    }
    impl Engine for SabotageEngine {
        fn dump(&self, _ctx: &DumpCtx) -> Result<std::path::PathBuf> {
            let conn = rusqlite::Connection::open(&self.db_path).unwrap();
            conn.execute_batch("DROP TABLE runs;").unwrap();
            anyhow::bail!("connection refused")
        }
        // Pipeline test double: dump-only, restore/verify are never exercised here.
        fn restore(&self, _ctx: &RestoreCtx) -> Result<()> {
            unreachable!()
        }
        fn verify(&self, _ctx: &VerifyCtx) -> Result<String> {
            unreachable!()
        }
    }

    fn setup() -> (Store, crate::store::SourceRow, tempfile::TempDir) {
        let st = Store::open(":memory:", MasterKey::from_hex(K).unwrap()).unwrap();
        st.add_source(&NewSource {
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
            settings: serde_json::json!({}),
            secrets: HashMap::new(),
        })
        .unwrap();
        let src = st.get_source("acme-db").unwrap();
        (st, src, tempfile::tempdir().unwrap())
    }

    #[test]
    fn success_path_backs_up_forgets_and_journals() {
        let (st, src, staging) = setup();
        let repo = MockRepo::default();
        let out = run_backup(&st, &repo, &src, staging.path(), &OkEngine).unwrap();
        assert_eq!(out.status, "success");
        assert_eq!(out.snapshot_id.as_deref(), Some("snap1"));
        assert_eq!(
            *repo.calls.borrow(),
            vec!["backup:source=acme-db:acme-db", "forget:source=acme-db"]
        );
        assert!(!staging.path().join("acme-db").exists(), "staging cleaned");
        let runs = st.recent_runs(1).unwrap();
        assert_eq!(runs[0].status, "success");
        assert_eq!(runs[0].bytes, Some(4));
    }

    #[test]
    fn mirror_engine_backs_up_mirror_and_it_survives() {
        let (st, src, staging) = setup();
        let repo = MockRepo::default();
        let out = run_backup(&st, &repo, &src, staging.path(), &MirrorEngine).unwrap();
        assert_eq!(out.status, "success");
        let mirror = staging.path().join(".mirrors").join("acme-db");
        assert!(
            mirror.join("obj1").exists(),
            "mirror persists after the run"
        );
        assert!(
            !staging.path().join("acme-db").exists(),
            "staging still cleaned"
        );
    }

    #[test]
    fn failure_path_journals_detail_and_cleans_staging() {
        let (st, src, staging) = setup();
        let repo = MockRepo::default();
        let err = run_backup(&st, &repo, &src, staging.path(), &FailEngine).unwrap_err();
        assert!(err.to_string().contains("connection refused"));
        assert!(
            repo.calls.borrow().is_empty(),
            "no restic calls on dump failure"
        );
        assert!(!staging.path().join("acme-db").exists(), "staging cleaned");
        let runs = st.recent_runs(1).unwrap();
        assert_eq!(runs[0].status, "failed");
        assert!(runs[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("connection refused"));
    }

    #[test]
    fn prune_failure_after_successful_backup_is_partial_success() {
        let (st, src, staging) = setup();
        let out = run_backup(&st, &PruneFailRepo, &src, staging.path(), &OkEngine).unwrap();
        assert_eq!(out.status, "success_prune_failed");
        assert_eq!(out.snapshot_id.as_deref(), Some("snap9"));
        let runs = st.recent_runs(1).unwrap();
        assert_eq!(runs[0].status, "success_prune_failed");
        assert_eq!(runs[0].snapshot_id.as_deref(), Some("snap9"));
        assert!(runs[0].detail.as_deref().unwrap().contains("locked"));
        assert_eq!(runs[0].bytes, Some(7));
    }

    #[test]
    fn repo_init_failure_is_journaled() {
        let (st, src, staging) = setup();
        let err = run_backup(&st, &InitFailRepo, &src, staging.path(), &OkEngine).unwrap_err();
        assert!(err.to_string().contains("unreachable"));
        let runs = st.recent_runs(1).unwrap();
        assert_eq!(runs[0].status, "failed");
        assert!(runs[0].detail.as_deref().unwrap().contains("unreachable"));
    }

    #[test]
    fn journal_write_failure_preserves_original_error() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("vk.db");
        let st = Store::open(db_path.to_str().unwrap(), MasterKey::from_hex(K).unwrap()).unwrap();
        st.add_source(&NewSource {
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
            settings: serde_json::json!({}),
            secrets: std::collections::HashMap::new(),
        })
        .unwrap();
        let src = st.get_source("acme-db").unwrap();
        let staging = tempfile::tempdir().unwrap();
        let err = run_backup(
            &st,
            &MockRepo::default(),
            &src,
            staging.path(),
            &SabotageEngine { db_path },
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("connection refused"),
            "original error must survive a journal write failure, got: {err:#}"
        );
    }
}
