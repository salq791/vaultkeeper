pub mod mongodb;
pub mod postgres;
pub mod supabase_functions;
pub mod supabase_storage;

use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;

pub struct DumpCtx {
    pub source_name: String,
    pub staging_dir: PathBuf,
    /// Runtime-only directory for short-lived credential files. Container
    /// deployments mount this as tmpfs so credentials never enter staging.
    pub secret_temp_dir: PathBuf,
    /// Persistent per-source mirror directory; read by engines that keep a
    /// reusable local mirror across runs instead of re-dumping from scratch
    /// each time (e.g. supabase_storage's rclone sync target).
    pub mirror_root: PathBuf,
    pub settings: serde_json::Value,
    pub secrets: HashMap<String, String>,
}

// No Debug derive: secrets carries restore-target credentials (e.g. a
// mongodb uri or postgres password embedded in target), and a Debug impl
// would let any future {:?} or dbg!() leak them into logs.
pub struct RestoreCtx {
    pub restored_dir: PathBuf,
    /// Durable destination for engines that restore to files rather than a
    /// live service (currently Supabase Edge Functions).
    pub durable_output_dir: PathBuf,
    pub secret_temp_dir: PathBuf,
    pub source_name: String,
    pub target: Option<String>,
    pub force_same_host: bool,
    pub confirm_remote_overwrite: bool,
    pub confirmed_source: Option<String>,
    pub settings: serde_json::Value,
    pub secrets: HashMap<String, String>,
}

// No Debug derive: scratch_postgres/scratch_mongodb carry scratch-database
// credentials embedded in the URL, and secrets carries restore-target
// credentials; a Debug impl would let any future {:?} or dbg!() leak them
// into logs.
pub struct VerifyCtx {
    pub restored_dir: PathBuf,
    pub secret_temp_dir: PathBuf,
    pub source_name: String,
    pub scratch_postgres: Option<String>,
    pub scratch_mongodb: Option<String>,
    pub settings: serde_json::Value,
    #[allow(dead_code)]
    // Still unread after Task 6's CLI/exec wiring: execute_verify populates
    // this from source.secrets, but no verify() implementation reads it back
    // (scratch credentials live in scratch_postgres/scratch_mongodb
    // instead). Kept for parity with RestoreCtx and future engines that may
    // need source secrets during verify. Verified empirically: removing this
    // allow makes `cargo clippy -D warnings` fail with "field `secrets` is
    // never read".
    pub secrets: HashMap<String, String>,
}

pub trait Engine {
    /// Produce the backup payload; return the directory restic should snapshot.
    fn dump(&self, ctx: &DumpCtx) -> Result<PathBuf>;
    /// Restore a previously-dumped payload back to a live target.
    fn restore(&self, ctx: &RestoreCtx) -> Result<()>;
    /// Restore into a scratch target and assert basic sanity; returns a
    /// metrics line journaled as detail on success.
    fn verify(&self, ctx: &VerifyCtx) -> Result<String>;
}

pub fn engine_for(kind: &str) -> Result<Box<dyn Engine>> {
    match kind {
        "postgres" => Ok(Box::new(postgres::PostgresEngine)),
        "mongodb" => Ok(Box::new(mongodb::MongodbEngine)),
        "supabase_storage" => Ok(Box::new(supabase_storage::SupabaseStorageEngine)),
        "supabase_functions" => Ok(Box::new(supabase_functions::SupabaseFunctionsEngine)),
        other => bail!("unknown engine kind: {other}"),
    }
}

/// Validate engine-specific settings and secret presence without making a
/// network request or printing secret values.
pub fn validate_config(
    kind: &str,
    settings: &serde_json::Value,
    secrets: &HashMap<String, String>,
) -> Result<()> {
    anyhow::ensure!(
        settings.is_object(),
        "engine settings must be a JSON object"
    );
    if let Some(timeout) = settings.get("timeout_minutes") {
        let minutes = timeout
            .as_u64()
            .context("timeout_minutes must be an integer")?;
        anyhow::ensure!(minutes >= 1, "timeout_minutes must be at least 1");
    }
    match kind {
        "postgres" => {
            postgres::pg_dump_invocation(settings, secrets, std::path::Path::new("db.dump"))?;
        }
        "mongodb" => {
            let uri = secrets
                .get("uri")
                .ok_or_else(|| anyhow::anyhow!("mongodb secrets missing 'uri'"))?;
            mongodb::uri_endpoints(uri)?;
            mongodb::mongodump_invocation(
                settings,
                secrets,
                std::path::Path::new("staging"),
                std::path::Path::new("mongo-config.yml"),
            )?;
        }
        "supabase_storage" => {
            supabase_storage::rclone_invocation(settings, secrets, std::path::Path::new("mirror"))?;
        }
        "supabase_functions" => {
            let project_ref = settings
                .get("project_ref")
                .and_then(serde_json::Value::as_str)
                .context("supabase_functions settings missing 'project_ref'")?;
            anyhow::ensure!(
                !project_ref.trim().is_empty(),
                "project_ref cannot be empty"
            );
            let functions_dir = settings
                .get("local_functions_dir")
                .and_then(serde_json::Value::as_str)
                .context("supabase_functions settings missing 'local_functions_dir'")?;
            anyhow::ensure!(
                std::path::Path::new(functions_dir).is_dir(),
                "local_functions_dir is not an accessible directory"
            );
            let token = secrets
                .get("access_token")
                .context("supabase_functions secrets missing 'access_token'")?;
            anyhow::ensure!(!token.is_empty(), "access_token cannot be empty");
            if let Some(api_base) = settings.get("api_base") {
                let api_base = api_base
                    .as_str()
                    .context("supabase_functions api_base must be a string")?;
                let parsed = url::Url::parse(api_base).context("invalid api_base URL")?;
                anyhow::ensure!(
                    matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some(),
                    "api_base must be an http(s) URL with a host"
                );
            }
        }
        other => bail!("unknown engine kind: {other}"),
    }
    Ok(())
}

pub fn required_tools(kind: &str) -> Result<&'static [&'static str]> {
    match kind {
        "postgres" => Ok(&["pg_dump", "pg_restore", "psql"]),
        "mongodb" => Ok(&["mongodump", "mongorestore"]),
        "supabase_storage" => Ok(&["rclone"]),
        "supabase_functions" => Ok(&["supabase"]),
        other => bail!("unknown engine kind: {other}"),
    }
}

/// Database restores are destructive even when the target is a different
/// host. Require an exact, typed source-name acknowledgement at the engine
/// boundary so every caller (CLI, TUI, or future API) receives the guard.
pub fn require_database_restore_confirmation(ctx: &RestoreCtx) -> Result<()> {
    anyhow::ensure!(
        ctx.confirmed_source.as_deref() == Some(ctx.source_name.as_str()),
        "database restore is destructive; pass --confirm-source {} to proceed",
        ctx.source_name
    );
    Ok(())
}

/// Env vars scrubbed from every engine child. Restic is the one exception
/// for the AWS vars: an S3-backed restic repo legitimately needs them.
pub const SCRUBBED_ENV_VARS: [&str; 7] = [
    "VAULTKEEPER_MASTER_KEY",
    "VAULTKEEPER_RESTORE_TARGET",
    "RESTIC_PASSWORD",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_PROFILE",
];

/// Strips vaultkeeper's own master key and any restic/AWS credentials from a
/// child command's inherited environment. Every engine child (pg_dump,
/// pg_restore, psql, mongodump, mongorestore, rclone, supabase) calls this
/// after the command is otherwise built, so none of them can read secrets
/// meant only for restic or vaultkeeper itself.
pub fn scrub_child_env(cmd: &mut std::process::Command) {
    for var in SCRUBBED_ENV_VARS {
        cmd.env_remove(var);
    }
}

/// Per-source child process deadline, read from the source's settings JSON
/// (key `timeout_minutes`); defaults to 60 minutes when absent.
pub fn timeout_from_settings(settings: &serde_json::Value) -> std::time::Duration {
    let mins = settings
        .get("timeout_minutes")
        .and_then(|v| v.as_u64())
        .unwrap_or(60);
    std::time::Duration::from_secs(mins.saturating_mul(60))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_engine_is_error() {
        assert!(engine_for("clippydb").is_err());
    }

    #[test]
    fn postgres_engine_resolves() {
        assert!(engine_for("postgres").is_ok());
    }

    #[test]
    fn all_engines_resolve() {
        for kind in [
            "postgres",
            "mongodb",
            "supabase_storage",
            "supabase_functions",
        ] {
            assert!(engine_for(kind).is_ok(), "{kind} should resolve");
        }
    }

    #[test]
    fn timeout_defaults_to_60_minutes() {
        assert_eq!(
            timeout_from_settings(&serde_json::json!({})),
            std::time::Duration::from_secs(3600)
        );
    }

    #[test]
    fn timeout_reads_settings_override() {
        assert_eq!(
            timeout_from_settings(&serde_json::json!({"timeout_minutes": 5})),
            std::time::Duration::from_secs(300)
        );
    }

    #[test]
    fn timeout_saturates_on_absurd_values() {
        assert_eq!(
            timeout_from_settings(&serde_json::json!({"timeout_minutes": u64::MAX})),
            std::time::Duration::from_secs(u64::MAX)
        );
    }

    #[test]
    fn scrub_list_covers_restore_vault_and_aws_secrets() {
        for var in [
            "VAULTKEEPER_MASTER_KEY",
            "VAULTKEEPER_RESTORE_TARGET",
            "RESTIC_PASSWORD",
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "AWS_PROFILE",
        ] {
            assert!(SCRUBBED_ENV_VARS.contains(&var), "{var} must be scrubbed");
        }

        let mut command = std::process::Command::new("unused");
        scrub_child_env(&mut command);
        for var in SCRUBBED_ENV_VARS {
            assert!(
                command
                    .get_envs()
                    .any(|(name, value)| name == var && value.is_none()),
                "{var} must be explicitly removed from child environments"
            );
        }
    }
}
