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

pub trait Engine {
    /// Produce the backup payload; return the directory restic should snapshot.
    fn dump(&self, ctx: &DumpCtx) -> Result<PathBuf>;
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
}
