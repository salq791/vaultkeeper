use crate::notify::{Notifier, RunEvent};
use crate::{config, crypto, engines, pipeline, restic, store};
use anyhow::Result;

pub fn execute_source(
    cfg: &config::Config,
    db_path: &str,
    source_name: &str,
) -> Result<pipeline::RunOutcome> {
    let st = store::Store::open(db_path, crypto::MasterKey::from_env()?)?;
    let source = st.get_source(source_name)?;
    let engine = engines::engine_for(&source.engine)?;
    let mut repo = restic::ResticCli::new(
        cfg.global.restic_repo.clone(),
        cfg.global.restic_password.clone(),
    );
    if let Some(mins) = cfg.global.restic_timeout_minutes {
        repo = repo.with_timeout(std::time::Duration::from_secs(mins * 60));
    }
    let notifier = Notifier::from_notify(&cfg.notify)?;
    notifier.notify(
        &source.name,
        source.healthchecks_uuid.as_deref(),
        &RunEvent::Started,
    );

    use crate::restic::Repo as _;
    repo.ensure_init()?;
    let result = pipeline::run_backup(
        &st,
        &repo,
        &source,
        &cfg.global.staging_dir,
        engine.as_ref(),
    );
    match &result {
        Ok(outcome) => {
            let detail = st
                .recent_runs(1)
                .ok()
                .and_then(|r| r.first().and_then(|row| row.detail.clone()));
            notifier.notify(
                &source.name,
                source.healthchecks_uuid.as_deref(),
                &RunEvent::Finished {
                    status: &outcome.status,
                    snapshot_id: outcome.snapshot_id.as_deref(),
                    detail: detail.as_deref(),
                },
            );
        }
        Err(e) => {
            let detail = crate::util::truncate_marked(&format!("{e:#}"), 2000);
            notifier.notify(
                &source.name,
                source.healthchecks_uuid.as_deref(),
                &RunEvent::Finished {
                    status: "failed",
                    snapshot_id: None,
                    detail: Some(&detail),
                },
            );
        }
    }
    result
}
