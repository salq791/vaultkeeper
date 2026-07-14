pub mod mongodb;
pub mod postgres;
pub mod supabase_functions;
pub mod supabase_storage;

use anyhow::{bail, Result};
use std::collections::HashMap;
use std::path::PathBuf;

pub struct DumpCtx {
    pub staging_dir: PathBuf,
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
    pub source_name: String,
    pub target: Option<String>,
    pub force_same_host: bool,
    pub confirm_remote_overwrite: bool,
    pub settings: serde_json::Value,
    pub secrets: HashMap<String, String>,
}

// No Debug derive: scratch_postgres/scratch_mongodb carry scratch-database
// credentials embedded in the URL, and secrets carries restore-target
// credentials; a Debug impl would let any future {:?} or dbg!() leak them
// into logs.
pub struct VerifyCtx {
    pub restored_dir: PathBuf,
    pub source_name: String,
    pub scratch_postgres: Option<String>,
    pub scratch_mongodb: Option<String>,
    pub settings: serde_json::Value,
    #[allow(dead_code)]
    // No current verify implementation needs source secrets (scratch
    // credentials live in scratch_postgres/scratch_mongodb instead); kept
    // for parity with RestoreCtx and future engines that may need them.
    pub secrets: HashMap<String, String>,
}

pub trait Engine {
    /// Produce the backup payload; return the directory restic should snapshot.
    fn dump(&self, ctx: &DumpCtx) -> Result<PathBuf>;
    /// Restore a previously-dumped payload back to a live target.
    #[allow(dead_code)]
    // Consumed by Task 6: CLI restore wiring.
    fn restore(&self, ctx: &RestoreCtx) -> Result<()>;
    /// Restore into a scratch target and assert basic sanity; returns a
    /// metrics line journaled as detail on success.
    #[allow(dead_code)]
    // Consumed by Task 6: CLI/scheduled verify wiring.
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
}
