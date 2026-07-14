pub mod postgres;

use anyhow::{bail, Result};
use std::collections::HashMap;
use std::path::PathBuf;

pub struct DumpCtx {
    pub staging_dir: PathBuf,
    /// Persistent per-source mirror directory; not yet read by any built-in
    /// engine, but available for engines that keep a reusable local mirror
    /// across runs instead of re-dumping from scratch each time.
    #[allow(dead_code)]
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
}
