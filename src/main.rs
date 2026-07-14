mod config;
mod crypto;
mod engines;
mod pipeline;
mod restic;
mod store;
mod types;

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
    tracing_subscriber::fmt().init();
    let cli = Cli::parse();
    match cli.command {
        Command::Source { cmd } => match cmd {
            SourceCmd::Add {
                name,
                engine,
                schedule,
                settings_json,
                secrets_json,
                retention,
                healthchecks_uuid,
            } => {
                engines::engine_for(&engine)?;
                let secrets_json = if secrets_json == "-" {
                    let mut buf = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)
                        .context("failed to read secrets JSON from stdin")?;
                    buf
                } else {
                    secrets_json
                };
                let st = open_store()?;
                st.add_source(&store::NewSource {
                    name: name.clone(),
                    engine,
                    schedule,
                    verify_schedule: None,
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
        },
        Command::Run { source } => {
            let cfg = config::load(&config_path())?;
            let st = open_store()?;
            let src = st.get_source(&source)?;
            let engine = engines::engine_for(&src.engine)?;
            let repo = restic::ResticCli::new(cfg.global.restic_repo, cfg.global.restic_password);
            use restic::Repo as _;
            repo.ensure_init()?;
            let out =
                pipeline::run_backup(&st, &repo, &src, &cfg.global.staging_dir, engine.as_ref())?;
            println!(
                "backup of {source} complete, snapshot {}",
                out.snapshot_id.unwrap_or_default()
            );
            Ok(())
        }
        Command::Snapshots { source } => {
            let cfg = config::load(&config_path())?;
            let repo = restic::ResticCli::new(cfg.global.restic_repo, cfg.global.restic_password);
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
            for tool in ["restic", "pg_dump"] {
                println!(
                    "{tool}: {}",
                    if tool_on_path(tool) {
                        "found"
                    } else {
                        "MISSING from PATH"
                    }
                );
            }
            Ok(())
        }
    }
}
