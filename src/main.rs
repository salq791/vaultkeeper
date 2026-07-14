mod config;
mod crypto;
mod engines;
mod exec;
mod notify;
mod pipeline;
mod restic;
mod schedule;
mod scheduler;
mod store;
mod types;
mod util;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "vaultkeeper", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Manage backup sources
    Source {
        #[command(subcommand)]
        cmd: SourceCmd,
    },
    /// Run a backup now
    Run {
        #[arg(long)]
        source: String,
    },
    /// List snapshots in the repository
    Snapshots {
        #[arg(long)]
        source: Option<String>,
    },
    /// Validate configuration, database, and required tools
    CheckConfig,
    /// Run the scheduler daemon
    Daemon,
    /// Restore a snapshot into a target database
    Restore {
        #[arg(long)]
        source: String,
        #[arg(long)]
        snapshot: Option<String>,
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        force_same_host: bool,
        #[arg(long)]
        confirm_remote_overwrite: bool,
    },
    /// Restore the latest snapshot into scratch databases and check it
    Verify {
        #[arg(long)]
        source: String,
    },
}

#[derive(Subcommand)]
enum SourceCmd {
    Add {
        #[arg(long)]
        name: String,
        #[arg(long)]
        engine: String,
        #[arg(long)]
        schedule: String,
        /// Cron schedule for scheduled restore verification; omit to leave verification unscheduled
        #[arg(long)]
        verify_schedule: Option<String>,
        #[arg(long)]
        settings_json: String,
        /// JSON object of secret values, or '-' to read the JSON from stdin (recommended; keeps secrets out of argv and shell history)
        #[arg(long)]
        secrets_json: String,
        /// daily,weekly,monthly (default 7,4,6)
        #[arg(long, default_value = "7,4,6")]
        retention: String,
        #[arg(long)]
        healthchecks_uuid: Option<String>,
    },
    List,
    Enable {
        #[arg(long)]
        name: String,
    },
    Disable {
        #[arg(long)]
        name: String,
    },
}

fn db_path() -> String {
    std::env::var("VAULTKEEPER_DB").unwrap_or_else(|_| "/data/vaultkeeper.db".into())
}

fn config_path() -> PathBuf {
    std::env::var("VAULTKEEPER_CONFIG")
        .unwrap_or_else(|_| "/config/config.toml".into())
        .into()
}

fn open_store() -> Result<store::Store> {
    store::Store::open(&db_path(), crypto::MasterKey::from_env()?)
}

fn parse_retention(s: &str) -> Result<types::Retention> {
    let parts: Vec<u32> = s
        .split(',')
        .map(|p| {
            p.trim()
                .parse::<u32>()
                .context("retention must be daily,weekly,monthly numbers")
        })
        .collect::<Result<_>>()?;
    anyhow::ensure!(
        parts.len() == 3,
        "retention must have exactly three numbers: daily,weekly,monthly"
    );
    Ok(types::Retention {
        daily: parts[0],
        weekly: parts[1],
        monthly: parts[2],
    })
}

fn tool_on_path(name: &str) -> bool {
    which_path(name).is_some()
}

fn which_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for candidate in [dir.join(name), dir.join(format!("{name}.exe"))] {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let cli = Cli::parse();
    match cli.command {
        Command::Source { cmd } => match cmd {
            SourceCmd::Add {
                name,
                engine,
                schedule,
                verify_schedule,
                settings_json,
                secrets_json,
                retention,
                healthchecks_uuid,
            } => {
                engines::engine_for(&engine)?;
                schedule::validate(&schedule)?;
                if let Some(vs) = &verify_schedule {
                    schedule::validate(vs)?;
                }
                let secrets_json = if secrets_json == "-" {
                    let mut buf = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)
                        .context("failed to read secrets JSON from stdin")?;
                    buf
                } else {
                    eprintln!(
                        "warning: inline --secrets-json exposes secrets to the process table and shell history; prefer --secrets-json -"
                    );
                    secrets_json
                };
                let st = open_store()?;
                st.add_source(&store::NewSource {
                    name: name.clone(),
                    engine,
                    schedule,
                    verify_schedule,
                    retention: parse_retention(&retention)?,
                    healthchecks_uuid,
                    settings: serde_json::from_str(&settings_json)
                        .context("invalid --settings-json")?,
                    secrets: serde_json::from_str::<HashMap<String, String>>(&secrets_json)
                        .map_err(|_| {
                            anyhow::anyhow!("invalid --secrets-json: pass a JSON object of string values (content not shown)")
                        })?,
                })?;
                println!("added source {name}");
                Ok(())
            }
            SourceCmd::List => {
                let st = open_store()?;
                for s in st.list_sources()? {
                    println!(
                        "{}\t{}\t{}\t{}",
                        s.name,
                        s.engine,
                        s.schedule,
                        if s.enabled { "enabled" } else { "disabled" }
                    );
                }
                Ok(())
            }
            SourceCmd::Enable { name } => {
                let st = open_store()?;
                st.set_enabled(&name, true)?;
                println!("{name} enabled");
                Ok(())
            }
            SourceCmd::Disable { name } => {
                let st = open_store()?;
                st.set_enabled(&name, false)?;
                println!("{name} disabled");
                Ok(())
            }
        },
        Command::Run { source } => {
            let cfg = config::load(&config_path())?;
            let out = exec::execute_source(&cfg, &db_path(), &source)?;
            println!(
                "backup of {source} complete, snapshot {}",
                out.snapshot_id.unwrap_or_default()
            );
            Ok(())
        }
        Command::Snapshots { source } => {
            let cfg = config::load(&config_path())?;
            let mut repo =
                restic::ResticCli::new(cfg.global.restic_repo, cfg.global.restic_password);
            if let Some(mins) = cfg.global.restic_timeout_minutes {
                repo = repo.with_timeout(std::time::Duration::from_secs(mins.saturating_mul(60)));
            }
            use restic::Repo as _;
            let tag = source.map(|s| format!("source={s}"));
            for snap in repo.snapshots(tag.as_deref())? {
                println!("{}\t{}\t{}", snap.id, snap.time, snap.tags.join(","));
            }
            Ok(())
        }
        Command::CheckConfig => {
            let cfg = config::load(&config_path())?;
            let st = open_store()?;
            let sources = st.list_sources()?;
            println!("config ok: staging={}", cfg.global.staging_dir.display());
            println!("db ok: {} sources", sources.len());
            let mut problems = 0usize;
            for src in &sources {
                for (label, expr) in [
                    ("schedule", Some(src.schedule.as_str())),
                    ("verify_schedule", src.verify_schedule.as_deref()),
                ] {
                    let Some(expr) = expr else { continue };
                    match schedule::validate(expr) {
                        Ok(()) => println!("{}: {label} ok", src.name),
                        Err(e) => {
                            println!("{}: {label} INVALID: {e}", src.name);
                            problems += 1;
                        }
                    }
                }
            }
            for tool in ["restic", "pg_dump", "mongodump", "rclone", "supabase"] {
                let found = tool_on_path(tool);
                if !found {
                    problems += 1;
                }
                println!(
                    "{tool}: {}",
                    if found { "found" } else { "MISSING from PATH" }
                );
            }
            let mut channels = Vec::new();
            if cfg.notify.healthchecks_base.is_some() {
                channels.push("healthchecks configured");
            }
            if cfg.notify.webhook_url.is_some() {
                channels.push("webhook configured");
            }
            if cfg.notify.ses.is_some() {
                channels.push("ses configured");
            }
            if channels.is_empty() {
                println!("notify: none configured");
            } else {
                println!("notify: {}", channels.join(", "));
            }
            let mut scratch = Vec::new();
            if cfg.verify.postgres_url.is_some() {
                scratch.push("postgres scratch configured");
            }
            if cfg.verify.mongodb_uri.is_some() {
                scratch.push("mongodb scratch configured");
            }
            if scratch.is_empty() {
                println!("verify: no scratch databases configured");
            } else {
                println!("verify: {}", scratch.join(", "));
            }
            anyhow::ensure!(problems == 0, "check-config found {problems} problem(s)");
            Ok(())
        }
        Command::Daemon => {
            let cfg = config::load(&config_path())?;
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            rt.block_on(scheduler::run_daemon(cfg, db_path()))
        }
        Command::Restore {
            source,
            snapshot,
            target,
            force_same_host,
            confirm_remote_overwrite,
        } => {
            let cfg = config::load(&config_path())?;
            let target = match target {
                Some(t) => {
                    eprintln!(
                        "warning: inline --target exposes the database password to the process table and shell history; prefer VAULTKEEPER_RESTORE_TARGET"
                    );
                    Some(t)
                }
                None => std::env::var("VAULTKEEPER_RESTORE_TARGET").ok(),
            };
            exec::execute_restore(
                &cfg,
                &db_path(),
                &source,
                snapshot.as_deref(),
                target.as_deref(),
                force_same_host,
                confirm_remote_overwrite,
            )?;
            println!("restore of {source} complete");
            Ok(())
        }
        Command::Verify { source } => {
            let cfg = config::load(&config_path())?;
            let out = exec::execute_verify(&cfg, &db_path(), &source)?;
            let detail = open_store()
                .and_then(|st| st.run_detail(out.run_id))
                .ok()
                .flatten();
            match detail {
                Some(d) => println!("verify of {source}: {} ({d})", out.status),
                None => println!("verify of {source}: {}", out.status),
            }
            Ok(())
        }
    }
}
