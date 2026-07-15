use crate::notify::{Notifier, RunEvent};
use crate::{config, crypto, engines, pipeline, restic, store};
use anyhow::Result;

/// Build a `ResticCli` from global config, applying the configured timeout
/// override when present. Shared by `execute_source`/`execute_verify`/
/// `execute_restore` here and by the scheduler's boot-time reachability
/// check, so the construction logic lives in exactly one place.
pub(crate) fn build_repo(cfg: &config::Config) -> restic::ResticCli {
    let mut repo = restic::ResticCli::new(
        cfg.global.restic_repo.clone(),
        cfg.global.restic_password.clone(),
        cfg.global.restic_host.clone(),
    );
    if let Some(mins) = cfg.global.restic_timeout_minutes {
        repo = repo.with_timeout(std::time::Duration::from_secs(mins.saturating_mul(60)));
    }
    repo
}

/// Layout for a restore/verify working directory under the staging root:
/// `<staging>/.{kind}/<name>`, e.g. `.verify/acme-db` or `.restore/acme-db`.
pub fn restore_workdir(
    staging_dir: &std::path::Path,
    kind: &str,
    name: &str,
) -> std::path::PathBuf {
    staging_dir.join(format!(".{kind}")).join(name)
}

/// Wipe `dir` if present and recreate it empty with owner-only permissions
/// on unix, so restic restore/engine restore-verify never inherit stale
/// files from a previous run.
fn fresh_workdir(dir: &std::path::Path) -> Result<()> {
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub fn execute_source(
    cfg: &config::Config,
    db_path: &str,
    source_name: &str,
) -> Result<pipeline::RunOutcome> {
    let st = store::Store::open(db_path, crypto::MasterKey::from_env()?)?;
    let source = st.get_source(source_name)?;
    let engine = engines::engine_for(&source.engine)?;
    let repo = build_repo(cfg);
    let notifier = Notifier::from_notify(&cfg.notify)?;
    notifier.notify(
        &source.name,
        source.healthchecks_uuid.as_deref(),
        &RunEvent::Started,
    );

    let result = pipeline::run_backup(
        &st,
        &repo,
        &source,
        &cfg.global.staging_dir,
        &cfg.global.secret_temp_dir,
        engine.as_ref(),
    );
    match &result {
        Ok(outcome) => {
            let detail = st.run_detail(outcome.run_id).ok().flatten();
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

/// Restore the latest snapshot into a fresh workdir and hand it to the
/// engine's `verify` for a restore-and-check against a scratch target.
/// Deliberately sends no `Started` ping: verifies are short enough (and
/// journaled immediately on completion) that a dead-man-switch style
/// "started" beacon adds noise without value here.
pub fn execute_verify(
    cfg: &config::Config,
    db_path: &str,
    source_name: &str,
) -> Result<pipeline::RunOutcome> {
    let st = store::Store::open(db_path, crypto::MasterKey::from_env()?)?;
    let source = st.get_source(source_name)?;
    let engine = engines::engine_for(&source.engine)?;
    let repo = build_repo(cfg);
    let notifier = Notifier::from_notify(&cfg.notify)?;
    let run_id = match st.start_run(source.id, "verify") {
        Ok(id) => id,
        Err(e) => {
            if e.to_string().contains("in progress") {
                let _ = st.record_skip(source.id, "verify", &e.to_string());
            }
            return Err(e);
        }
    };
    let heartbeat = match st.start_heartbeat(run_id) {
        Ok(heartbeat) => heartbeat,
        Err(error) => {
            let detail = crate::util::truncate_marked(&format!("{error:#}"), 4000);
            let _ = st.finish_run(run_id, "verify_failed", None, None, Some(&detail));
            return Err(error);
        }
    };

    let workdir = restore_workdir(&cfg.global.staging_dir, "verify", &source.name);
    let result = (|| -> Result<String> {
        use crate::restic::Repo as _;
        repo.ensure_init()?;
        let snap = restic::latest_snapshot(&repo, &format!("source={}", source.name))?;
        fresh_workdir(&workdir)?;
        repo.restore(&snap.id, &workdir)?;
        let ctx = engines::VerifyCtx {
            restored_dir: workdir.clone(),
            secret_temp_dir: cfg.global.secret_temp_dir.clone(),
            source_name: source.name.clone(),
            scratch_postgres: cfg.verify.postgres_url.clone(),
            scratch_mongodb: cfg.verify.mongodb_uri.clone(),
            settings: source.settings.clone(),
            secrets: source.secrets.clone(),
        };
        engine.verify(&ctx)
    })();
    drop(heartbeat);
    let _ = std::fs::remove_dir_all(&workdir);

    let (status, detail) = match &result {
        Ok(metrics) => ("verify_passed", metrics.clone()),
        Err(e) => (
            "verify_failed",
            crate::util::truncate_marked(&format!("{e:#}"), 4000),
        ),
    };
    if let Err(je) = st.finish_run(run_id, status, None, None, Some(&detail)) {
        tracing::warn!("failed to journal verify run {run_id}: {je:#}");
    }
    // Notify source.verify_healthchecks_uuid, NEVER the backup healthchecks_uuid:
    // the backup check measures backup freshness (a dead-man switch), and a
    // verify run pinging it would defeat that purpose by making the switch
    // look alive even when backups have stopped. When unset, verify sends no
    // healthchecks ping at all; webhook and SES notification are unaffected,
    // since those are gated on the run status, not on this uuid.
    notifier.notify(
        &source.name,
        source.verify_healthchecks_uuid.as_deref(),
        &RunEvent::Finished {
            status,
            snapshot_id: None,
            detail: Some(&detail),
        },
    );
    result.map(|_| pipeline::RunOutcome {
        run_id,
        snapshot_id: None,
        status: status.into(),
    })
}

/// Restore a snapshot (given or latest) into a fresh workdir and hand it to
/// the engine's `restore` to write back to a live target. Restores are
/// operator-driven and interactive (run from a terminal, watched live): the
/// outcome is journaled for history but deliberately not sent to any
/// notification channel.
#[allow(clippy::too_many_arguments)]
pub fn execute_restore(
    cfg: &config::Config,
    db_path: &str,
    source_name: &str,
    snapshot: Option<&str>,
    target: Option<&str>,
    force_same_host: bool,
    confirm_remote_overwrite: bool,
    confirmed_source: Option<&str>,
) -> Result<()> {
    let st = store::Store::open(db_path, crypto::MasterKey::from_env()?)?;
    let source = st.get_source(source_name)?;
    let engine = engines::engine_for(&source.engine)?;
    let repo = build_repo(cfg);
    let run_id = st.start_run(source.id, "restore")?;
    let heartbeat = match st.start_heartbeat(run_id) {
        Ok(heartbeat) => heartbeat,
        Err(error) => {
            let detail = crate::util::truncate_marked(&format!("{error:#}"), 4000);
            let _ = st.finish_run(run_id, "failed", None, None, Some(&detail));
            return Err(error);
        }
    };

    let workdir = restore_workdir(&cfg.global.staging_dir, "restore", &source.name);
    let result = (|| -> Result<()> {
        use crate::restic::Repo as _;
        repo.ensure_init()?;
        let snap_id = match snapshot {
            Some(id) => id.to_string(),
            None => restic::latest_snapshot(&repo, &format!("source={}", source.name))?.id,
        };
        fresh_workdir(&workdir)?;
        repo.restore(&snap_id, &workdir)?;
        let ctx = engines::RestoreCtx {
            restored_dir: workdir.clone(),
            durable_output_dir: durable_restore_dir(
                &cfg.global.restore_output_dir,
                &source.name,
                &snap_id,
            ),
            secret_temp_dir: cfg.global.secret_temp_dir.clone(),
            source_name: source.name.clone(),
            target: target.map(|t| t.to_string()),
            force_same_host,
            confirm_remote_overwrite,
            confirmed_source: confirmed_source.map(str::to_string),
            settings: source.settings.clone(),
            secrets: source.secrets.clone(),
        };
        engine.restore(&ctx)
    })();
    drop(heartbeat);
    let _ = std::fs::remove_dir_all(&workdir);

    match &result {
        Ok(()) => {
            if let Err(je) = st.finish_run(run_id, "success", None, None, None) {
                tracing::warn!("failed to journal restore run {run_id}: {je:#}");
            }
        }
        Err(e) => {
            let detail = crate::util::truncate_marked(&format!("{e:#}"), 4000);
            if let Err(je) = st.finish_run(run_id, "failed", None, None, Some(&detail)) {
                tracing::warn!("failed to journal restore run {run_id}: {je:#}");
            }
        }
    }
    result
}

/// Reclaim repository space after retention has removed snapshot references,
/// then verify repository integrity. This is scheduled separately from each
/// backup because prune can be expensive and requires an exclusive lock.
pub fn execute_maintenance(cfg: &config::Config) -> Result<()> {
    use crate::restic::Repo as _;

    let result = (|| {
        let repo = build_repo(cfg);
        repo.ensure_init()?;
        repo.prune()?;
        repo.check()?;
        Ok(())
    })();
    if let Err(error) = &result {
        let detail = crate::util::truncate_marked(&format!("{error:#}"), 2000);
        match Notifier::from_notify(&cfg.notify) {
            Ok(notifier) => notifier.notify(
                "repository-maintenance",
                None,
                &RunEvent::Finished {
                    status: "failed",
                    snapshot_id: None,
                    detail: Some(&detail),
                },
            ),
            Err(notify_error) => tracing::warn!(
                "could not construct notifier for repository maintenance failure: {notify_error:#}"
            ),
        }
    }
    result
}

/// Produce a traversal-safe, deterministic output directory for file-based
/// restores. Ordinary restic snapshot IDs remain readable; arbitrary
/// selectors are represented by a short SHA-256 label.
pub fn durable_restore_dir(
    root: &std::path::Path,
    source_name: &str,
    snapshot_selector: &str,
) -> std::path::PathBuf {
    use sha2::{Digest, Sha256};

    let safe = !snapshot_selector.is_empty()
        && snapshot_selector.len() <= 64
        && snapshot_selector
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
    let component = if safe {
        snapshot_selector.to_string()
    } else {
        let digest = hex::encode(Sha256::digest(snapshot_selector.as_bytes()));
        format!("selector-{}", &digest[..16])
    };
    root.join(source_name).join(component)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_workdir_layout() {
        let d = restore_workdir(std::path::Path::new("/staging"), "verify", "acme-db");
        assert!(d.ends_with(std::path::Path::new(".verify/acme-db")));
        let r = restore_workdir(std::path::Path::new("/staging"), "restore", "acme-db");
        assert!(r.ends_with(std::path::Path::new(".restore/acme-db")));
    }

    #[test]
    fn durable_restore_dir_hashes_unsafe_snapshot_selectors() {
        let path = durable_restore_dir(
            std::path::Path::new("/data/restores"),
            "acme-fns",
            "../../latest",
        );
        assert!(path.starts_with("/data/restores/acme-fns"));
        assert!(!path.to_string_lossy().contains(".."));
    }
}
