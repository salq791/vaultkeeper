pub mod postgres;

use anyhow::{bail, Result};
use std::collections::HashMap;
use std::path::PathBuf;

pub struct DumpCtx {
    pub staging_dir: PathBuf,
    pub settings: serde_json::Value,
    pub secrets: HashMap<String, String>,
}

pub trait Engine {
    fn dump(&self, ctx: &DumpCtx) -> Result<()>;
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
