use crate::{config, crypto, exec, schedule, store};
use anyhow::{Context, Result};
use chrono::{DateTime, Local};

pub fn sleep_duration(next: DateTime<Local>, now: DateTime<Local>) -> std::time::Duration {
    (next - now).to_std().unwrap_or(std::time::Duration::ZERO)
}

pub async fn run_daemon(cfg: config::Config, db_path: String) -> Result<()> {
    let st = store::Store::open(&db_path, crypto::MasterKey::from_env()?)?;
    let sources: Vec<_> = st
        .list_sources()?
        .into_iter()
        .filter(|s| s.enabled)
        .collect();
    drop(st);
    anyhow::ensure!(
        !sources.is_empty(),
        "no enabled sources; add one with 'vaultkeeper source add'"
    );
    for s in &sources {
        schedule::validate(&s.schedule).with_context(|| format!("source {}", s.name))?;
    }
    tracing::info!(
        "daemon starting with {} enabled source(s); source changes require a restart",
        sources.len()
    );

    let mut repo = crate::restic::ResticCli::new(
        cfg.global.restic_repo.clone(),
        cfg.global.restic_password.clone(),
    );
    if let Some(mins) = cfg.global.restic_timeout_minutes {
        repo = repo.with_timeout(std::time::Duration::from_secs(mins.saturating_mul(60)));
    }
    {
        use crate::restic::Repo as _;
        repo.ensure_init()
            .context("restic repository unreachable at daemon startup")?;
    }
    tracing::info!("restic repository reachable");

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let mut handles = Vec::new();
    let cfg = std::sync::Arc::new(cfg);
    for source in sources {
        let cfg = cfg.clone();
        let db_path = db_path.clone();
        let mut shutdown = shutdown_rx.clone();
        handles.push(tokio::spawn(async move {
            loop {
                let next = match schedule::next_occurrence(&source.schedule, Local::now()) {
                    Ok(n) => n,
                    Err(e) => {
                        tracing::error!(
                            "{}: schedule error, stopping this source: {e:#}",
                            source.name
                        );
                        return;
                    }
                };
                tracing::info!("{}: next run at {}", source.name, next);
                tokio::select! {
                    _ = tokio::time::sleep(sleep_duration(next, Local::now())) => {}
                    _ = shutdown.changed() => {
                        tracing::info!("{}: shutdown requested", source.name);
                        return;
                    }
                }
                let cfg2 = cfg.clone();
                let db2 = db_path.clone();
                let name = source.name.clone();
                let join =
                    tokio::task::spawn_blocking(move || exec::execute_source(&cfg2, &db2, &name));
                match join.await {
                    Ok(Ok(outcome)) => tracing::info!(
                        "{}: run finished with status {}",
                        source.name,
                        outcome.status
                    ),
                    Ok(Err(e)) => tracing::error!("{}: run failed: {e:#}", source.name),
                    Err(e) => tracing::error!("{}: run panicked: {e}", source.name),
                }
            }
        }));
    }

    tokio::signal::ctrl_c()
        .await
        .context("failed to listen for ctrl-c")?;
    tracing::info!("ctrl-c received: stopping schedules, waiting for in-flight runs");
    let _ = shutdown_tx.send(true);
    for h in handles {
        let _ = h.await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn sleep_duration_positive_and_clamped() {
        let now = chrono::Local.with_ymd_and_hms(2026, 1, 1, 1, 0, 0).unwrap();
        let next = chrono::Local.with_ymd_and_hms(2026, 1, 1, 2, 0, 0).unwrap();
        assert_eq!(
            sleep_duration(next, now),
            std::time::Duration::from_secs(3600)
        );
        assert_eq!(sleep_duration(now, next), std::time::Duration::ZERO);
    }
}
