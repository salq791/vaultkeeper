use crate::engines::{DumpCtx, Engine};
use crate::restic::Repo;
use crate::store::{SourceRow, Store};
use anyhow::Result;
use std::path::Path;

#[derive(Debug)]
pub struct RunOutcome {
    #[allow(dead_code)]
    pub run_id: i64,
    pub snapshot_id: Option<String>,
    #[allow(dead_code)]
    pub status: String,
}

pub fn run_backup(
    store: &Store,
    repo: &dyn Repo,
    source: &SourceRow,
    staging_root: &Path,
    engine: &dyn Engine,
) -> Result<RunOutcome> {
    let run_id = store.start_run(source.id, "backup")?;
    let staging_dir = staging_root.join(&source.name);
    let result = (|| -> Result<(String, i64)> {
        if staging_dir.exists() {
            std::fs::remove_dir_all(&staging_dir)?;
        }
        std::fs::create_dir_all(&staging_dir)?;
        let ctx = DumpCtx {
            staging_dir: staging_dir.clone(),
            settings: source.settings.clone(),
            secrets: source.secrets.clone(),
        };
        engine.dump(&ctx)?;
        let tag = format!("source={}", source.name);
        let summary = repo.backup(&staging_dir, &tag)?;
        repo.forget(&tag, &source.retention)?;
        Ok((summary.snapshot_id, summary.total_bytes_processed))
    })();
    let _ = std::fs::remove_dir_all(&staging_dir);

    match result {
        Ok((snapshot_id, bytes)) => {
            store.finish_run(run_id, "success", Some(bytes), Some(&snapshot_id), None)?;
            Ok(RunOutcome {
                run_id,
                snapshot_id: Some(snapshot_id),
                status: "success".into(),
            })
        }
        Err(e) => {
            let detail: String = format!("{e:#}").chars().take(4000).collect();
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
    use crate::engines::{DumpCtx, Engine};
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
        fn dump(&self, ctx: &DumpCtx) -> Result<()> {
            std::fs::write(ctx.staging_dir.join("db.dump"), b"data")?;
            Ok(())
        }
    }

    struct FailEngine;
    impl Engine for FailEngine {
        fn dump(&self, _ctx: &DumpCtx) -> Result<()> {
            bail!("connection refused");
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
            self.calls.borrow_mut().push(format!("backup:{tag}"));
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
            vec!["backup:source=acme-db", "forget:source=acme-db"]
        );
        assert!(!staging.path().join("acme-db").exists(), "staging cleaned");
        let runs = st.recent_runs(1).unwrap();
        assert_eq!(runs[0].status, "success");
        assert_eq!(runs[0].bytes, Some(4));
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
}
