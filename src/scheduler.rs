use crate::{config, crypto, exec, schedule, store};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;

pub fn sleep_duration(next: DateTime<Tz>, now: DateTime<Tz>) -> std::time::Duration {
    (next - now).to_std().unwrap_or(std::time::Duration::ZERO)
}

/// Project (name, verify_schedule) pairs for every source that has a verify
/// schedule configured, dropping the backup schedule and any source without
/// one.
pub fn verify_jobs(sources: &[(String, String, Option<String>)]) -> Vec<(String, String)> {
    sources
        .iter()
        .filter_map(|(name, _, vs)| vs.as_ref().map(|v| (name.clone(), v.clone())))
        .collect()
}

pub async fn run_daemon(cfg: config::Config, db_path: String) -> Result<()> {
    let timezone: Tz = cfg
        .global
        .timezone
        .parse()
        .with_context(|| format!("invalid IANA timezone '{}'", cfg.global.timezone))?;
    schedule::validate(&cfg.global.maintenance_schedule)
        .context("invalid global maintenance_schedule")?;
    let st = store::Store::open(&db_path, crypto::MasterKey::from_env()?)?;
    let cleared = st.reconcile_stale_running()?;
    if cleared > 0 {
        tracing::warn!("cleared {cleared} run row(s) whose heartbeat lease expired");
    }
    let fresh_running = st.count_running()?;
    if fresh_running > 0 {
        tracing::info!(
            "{} run row(s) still marked running with a fresh heartbeat",
            fresh_running
        );
    }
    let sources: Vec<(String, String, Option<String>)> = st
        .list_sources()?
        .into_iter()
        .filter(|s| s.enabled)
        .map(|s| (s.name, s.schedule, s.verify_schedule))
        .collect();
    drop(st);
    if sources.is_empty() {
        tracing::warn!(
            "no enabled sources configured; only repository maintenance will be scheduled"
        );
    }
    for (name, schedule, verify_schedule) in &sources {
        schedule::validate(schedule).with_context(|| format!("source {name}"))?;
        if let Some(vs) = verify_schedule {
            schedule::validate(vs).with_context(|| format!("source {name} verify_schedule"))?;
        }
    }
    let verify_jobs = verify_jobs(&sources);
    tracing::info!(
        "daemon starting with {} enabled source(s), {} scheduled verif{} in timezone {}; source changes require a restart",
        sources.len(),
        verify_jobs.len(),
        if verify_jobs.len() == 1 { "y" } else { "ies" },
        timezone
    );

    let repo = exec::build_repo(&cfg);
    {
        use crate::restic::Repo as _;
        repo.ensure_init()
            .context("restic repository unreachable at daemon startup")?;
    }
    tracing::info!("restic repository reachable");

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let mut handles = Vec::new();
    let cfg = std::sync::Arc::new(cfg);
    for (name, schedule_expr, _verify_schedule) in sources {
        let cfg = cfg.clone();
        let db_path = db_path.clone();
        let mut shutdown = shutdown_rx.clone();
        handles.push(tokio::spawn(async move {
            loop {
                let now = Utc::now().with_timezone(&timezone);
                let next = match schedule::next_occurrence(&schedule_expr, now) {
                    Ok(n) => n,
                    Err(e) => {
                        tracing::error!("{}: schedule error, stopping this source: {e:#}", name);
                        return;
                    }
                };
                tracing::info!("{}: next run at {}", name, next);
                tokio::select! {
                    _ = tokio::time::sleep(sleep_duration(next, Utc::now().with_timezone(&timezone))) => {}
                    _ = shutdown.changed() => {
                        tracing::info!("{}: shutdown requested", name);
                        return;
                    }
                }
                let cfg2 = cfg.clone();
                let db2 = db_path.clone();
                let name2 = name.clone();
                let join =
                    tokio::task::spawn_blocking(move || exec::execute_source(&cfg2, &db2, &name2));
                match join.await {
                    Ok(Ok(outcome)) => {
                        tracing::info!("{}: run finished with status {}", name, outcome.status)
                    }
                    Ok(Err(e)) => tracing::error!("{}: run failed: {e:#}", name),
                    Err(e) => tracing::error!("{}: run panicked: {e}", name),
                }
            }
        }));
    }

    for (name, schedule_expr) in verify_jobs {
        let cfg = cfg.clone();
        let db_path = db_path.clone();
        let mut shutdown = shutdown_rx.clone();
        handles.push(tokio::spawn(async move {
            loop {
                let now = Utc::now().with_timezone(&timezone);
                let next = match schedule::next_occurrence(&schedule_expr, now) {
                    Ok(n) => n,
                    Err(e) => {
                        tracing::error!(
                            "{}: schedule error, stopping this verify job: {e:#}",
                            name
                        );
                        return;
                    }
                };
                tracing::info!("{}: next verify at {}", name, next);
                tokio::select! {
                    _ = tokio::time::sleep(sleep_duration(next, Utc::now().with_timezone(&timezone))) => {}
                    _ = shutdown.changed() => {
                        tracing::info!("{}: shutdown requested", name);
                        return;
                    }
                }
                let cfg2 = cfg.clone();
                let db2 = db_path.clone();
                let name2 = name.clone();
                let join =
                    tokio::task::spawn_blocking(move || exec::execute_verify(&cfg2, &db2, &name2));
                match join.await {
                    Ok(Ok(outcome)) => {
                        tracing::info!("{}: verify finished with status {}", name, outcome.status)
                    }
                    Ok(Err(e)) => tracing::error!("{}: verify failed: {e:#}", name),
                    Err(e) => tracing::error!("{}: verify panicked: {e}", name),
                }
            }
        }));
    }

    {
        let cfg = cfg.clone();
        let mut shutdown = shutdown_rx.clone();
        let schedule_expr = cfg.global.maintenance_schedule.clone();
        handles.push(tokio::spawn(async move {
            loop {
                let now = Utc::now().with_timezone(&timezone);
                let next = match schedule::next_occurrence(&schedule_expr, now) {
                    Ok(next) => next,
                    Err(error) => {
                        tracing::error!("repository maintenance schedule error: {error:#}");
                        return;
                    }
                };
                tracing::info!("next repository prune/check at {}", next);
                tokio::select! {
                    _ = tokio::time::sleep(sleep_duration(next, Utc::now().with_timezone(&timezone))) => {}
                    _ = shutdown.changed() => {
                        tracing::info!("repository maintenance: shutdown requested");
                        return;
                    }
                }
                let cfg = cfg.clone();
                match tokio::task::spawn_blocking(move || exec::execute_maintenance(&cfg)).await {
                    Ok(Ok(())) => tracing::info!("repository prune/check completed"),
                    Ok(Err(error)) => {
                        tracing::error!("repository prune/check failed: {error:#}")
                    }
                    Err(error) => tracing::error!("repository prune/check panicked: {error}"),
                }
            }
        }));
    }

    shutdown_signal().await;
    let _ = shutdown_tx.send(true);
    for h in handles {
        let _ = h.await;
    }
    Ok(())
}

/// Waits for ctrl-c or, on unix, SIGTERM (what `docker stop` sends). Windows
/// only has ctrl-c, so the cfg(not(unix)) arm is the one that ever compiles
/// or runs on this development machine; the unix arm is verified by CI's
/// ubuntu jobs.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    tracing::info!("shutdown signal received: stopping schedules, waiting for in-flight runs");
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn sleep_duration_positive_and_clamped() {
        let now = chrono_tz::UTC
            .with_ymd_and_hms(2026, 1, 1, 1, 0, 0)
            .unwrap();
        let next = chrono_tz::UTC
            .with_ymd_and_hms(2026, 1, 1, 2, 0, 0)
            .unwrap();
        assert_eq!(
            sleep_duration(next, now),
            std::time::Duration::from_secs(3600)
        );
        assert_eq!(sleep_duration(now, next), std::time::Duration::ZERO);
    }

    #[test]
    fn verify_jobs_filters_sources_with_verify_schedules() {
        let sources = vec![
            (
                "a".to_string(),
                "0 2 * * *".to_string(),
                Some("0 5 * * 0".to_string()),
            ),
            ("b".to_string(), "0 3 * * *".to_string(), None),
        ];
        assert_eq!(
            verify_jobs(&sources),
            vec![("a".to_string(), "0 5 * * 0".to_string())]
        );
    }
}
