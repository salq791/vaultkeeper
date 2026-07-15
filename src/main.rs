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
mod tui;
mod types;
mod util;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::collections::{BTreeSet, HashMap};
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
    /// Explicitly initialize the configured restic repository
    InitRepository,
    /// Run the scheduler daemon
    Daemon,
    /// Restore a snapshot into a target database
    Restore {
        #[arg(long)]
        source: String,
        #[arg(long)]
        snapshot: Option<String>,
        #[arg(long)]
        force_same_host: bool,
        #[arg(long)]
        confirm_remote_overwrite: bool,
        /// Exact source name acknowledgement required for destructive database restores
        #[arg(long)]
        confirm_source: Option<String>,
    },
    /// Restore the latest snapshot into scratch databases and check it
    Verify {
        #[arg(long)]
        source: String,
    },
    /// Launch the interactive terminal UI
    Tui,
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
        /// Pass '-' and pipe a JSON object through stdin; inline secret values are refused
        #[arg(long)]
        secrets_json: String,
        /// daily,weekly,monthly (default 7,4,6)
        #[arg(long, default_value = "7,4,6")]
        retention: String,
        #[arg(long)]
        healthchecks_uuid: Option<String>,
        #[arg(long)]
        verify_healthchecks_uuid: Option<String>,
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

fn probe_writable_directory(path: &std::path::Path) -> Result<()> {
    util::ensure_private_dir(path)?;
    tempfile::Builder::new()
        .prefix(".vaultkeeper-write-check-")
        .tempfile_in(path)
        .with_context(|| format!("directory is not writable: {}", path.display()))?;
    Ok(())
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
                verify_healthchecks_uuid,
            } => {
                engines::engine_for(&engine)?;
                schedule::validate(&schedule)?;
                if let Some(vs) = &verify_schedule {
                    schedule::validate(vs)?;
                }
                anyhow::ensure!(
                    secrets_json == "-",
                    "inline secret values are refused because process arguments are observable; pipe JSON to --secrets-json -"
                );
                let mut secrets_json = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut secrets_json)
                    .context("failed to read secrets JSON from stdin")?;
                let settings: serde_json::Value =
                    serde_json::from_str(&settings_json).context("invalid --settings-json")?;
                let secrets = serde_json::from_str::<HashMap<String, String>>(&secrets_json)
                    .map_err(|_| {
                        anyhow::anyhow!("invalid --secrets-json: pass a JSON object of string values (content not shown)")
                    })?;
                engines::validate_config(&engine, &settings, &secrets)?;
                let st = open_store()?;
                st.add_source(&store::NewSource {
                    name: name.clone(),
                    engine,
                    schedule,
                    verify_schedule,
                    retention: types::Retention::parse_csv(&retention)?,
                    healthchecks_uuid,
                    verify_healthchecks_uuid,
                    settings,
                    secrets,
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
            let repo = exec::build_repo(&cfg);
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
            println!("db ok: {} sources", sources.len());
            let mut problems = 0usize;
            for (label, directory) in [
                ("staging", cfg.global.staging_dir.as_path()),
                ("secret_temp", cfg.global.secret_temp_dir.as_path()),
                ("restore_output", cfg.global.restore_output_dir.as_path()),
            ] {
                match probe_writable_directory(directory) {
                    Ok(()) => println!("{label}: writable ({})", directory.display()),
                    Err(error) => {
                        println!("{label}: INVALID: {error}");
                        problems += 1;
                    }
                }
            }
            if let Err(error) = cfg.global.timezone.parse::<chrono_tz::Tz>() {
                println!("timezone: INVALID: {error}");
                problems += 1;
            } else {
                println!("timezone: {}", cfg.global.timezone);
            }
            if let Err(error) = schedule::validate(&cfg.global.maintenance_schedule) {
                println!("maintenance_schedule: INVALID: {error}");
                problems += 1;
            } else {
                println!("maintenance_schedule: {}", cfg.global.maintenance_schedule);
            }
            let repo = exec::build_repo(&cfg);
            match repo.probe() {
                Ok(()) => println!("restic repository: reachable and initialized"),
                Err(error) => {
                    println!("restic repository: UNREACHABLE: {error}");
                    problems += 1;
                }
            }
            let mut required_tools = BTreeSet::from(["restic"]);
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
                if let Some(tm) = src.settings.get("timeout_minutes") {
                    match tm.as_u64() {
                        Some(v) if v >= 1 => println!("{}: timeout_minutes ok ({v})", src.name),
                        _ => {
                            println!(
                                "{}: timeout_minutes INVALID (must be an integer >= 1)",
                                src.name
                            );
                            problems += 1;
                        }
                    }
                }
                match engines::validate_config(&src.engine, &src.settings, &src.secrets) {
                    Ok(()) => println!("{}: engine config ok ({})", src.name, src.engine),
                    Err(error) => {
                        println!("{}: engine config INVALID: {error}", src.name);
                        problems += 1;
                    }
                }
                match engines::required_tools(&src.engine) {
                    Ok(tools) => required_tools.extend(tools.iter().copied()),
                    Err(error) => {
                        println!("{}: engine INVALID: {error}", src.name);
                        problems += 1;
                    }
                }
            }
            for tool in required_tools {
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
            if let Some(url) = &cfg.verify.postgres_url {
                match engines::postgres::parse_pg_url(url) {
                    Ok(_) => scratch.push("postgres scratch URL valid"),
                    Err(error) => {
                        println!("verify postgres: INVALID: {error}");
                        problems += 1;
                    }
                }
            }
            if let Some(uri) = &cfg.verify.mongodb_uri {
                match engines::mongodb::uri_endpoints(uri) {
                    Ok(_) => scratch.push("mongodb scratch URI valid"),
                    Err(error) => {
                        println!("verify mongodb: INVALID: {error}");
                        problems += 1;
                    }
                }
            }
            if scratch.is_empty() {
                println!("verify: no scratch databases configured");
            } else {
                println!("verify: {}", scratch.join(", "));
            }
            anyhow::ensure!(problems == 0, "check-config found {problems} problem(s)");
            Ok(())
        }
        Command::InitRepository => {
            let cfg = config::load(&config_path())?;
            let repo = exec::build_repo(&cfg);
            repo.initialize()?;
            println!("restic repository initialized");
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
            force_same_host,
            confirm_remote_overwrite,
            confirm_source,
        } => {
            let cfg = config::load(&config_path())?;
            let target = std::env::var("VAULTKEEPER_RESTORE_TARGET")
                .ok()
                .filter(|s| !s.is_empty());
            exec::execute_restore(
                &cfg,
                &db_path(),
                &source,
                snapshot.as_deref(),
                target.as_deref(),
                force_same_host,
                confirm_remote_overwrite,
                confirm_source.as_deref(),
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
        Command::Tui => {
            let cfg = config::load(&config_path())?;
            tui::run(cfg, db_path())
        }
    }
}
