use super::{DumpCtx, Engine};
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct MongodbEngine;

// No Debug derive: config_contents carries the raw secret uri, and a Debug
// impl would let any future {:?} or dbg!() leak credentials into logs.
pub struct MongoInvocation {
    pub argv: Vec<String>,
    pub config_path: PathBuf,
    pub config_contents: String,
}

pub fn mongodump_invocation(
    settings: &serde_json::Value,
    secrets: &HashMap<String, String>,
    staging_dir: &Path,
) -> Result<MongoInvocation> {
    let uri = secrets
        .get("uri")
        .context("mongodb secrets missing 'uri' (full connection string)")?;
    let config_path = staging_dir.join(".mongodump-config.yml");
    let out_dir = staging_dir.join("dump");
    let mut argv = vec![
        "--config".to_string(),
        config_path.display().to_string(),
        "--out".to_string(),
        out_dir.display().to_string(),
    ];
    if let Some(db) = settings.get("db").and_then(|v| v.as_str()) {
        argv.push("--db".to_string());
        argv.push(db.to_string());
    }
    Ok(MongoInvocation {
        argv,
        config_path,
        config_contents: format!("uri: {uri}\n"),
    })
}

impl Engine for MongodbEngine {
    fn dump(&self, ctx: &DumpCtx) -> Result<PathBuf> {
        let inv = mongodump_invocation(&ctx.settings, &ctx.secrets, &ctx.staging_dir)?;
        // staging_dir is wiped fresh by the pipeline each run, so create_new cannot collide
        crate::util::write_new_0600(&inv.config_path, inv.config_contents.as_bytes())?;
        let out = Command::new("mongodump")
            .args(&inv.argv)
            .env_remove("VAULTKEEPER_MASTER_KEY")
            .env_remove("RESTIC_PASSWORD")
            .output();
        let _ = std::fs::remove_file(&inv.config_path);
        if inv.config_path.exists() {
            bail!("mongodump config file could not be removed; aborting so credentials are not snapshotted");
        }
        let out =
            out.context("failed to spawn mongodump (is mongodb-database-tools installed?)")?;
        if !out.status.success() {
            bail!(
                "mongodump failed: {}",
                crate::util::truncate_marked(&String::from_utf8_lossy(&out.stderr), 2000)
            );
        }
        Ok(ctx.staging_dir.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::Path;

    fn secrets() -> HashMap<String, String> {
        HashMap::from([(
            "uri".to_string(),
            "mongodb://user:pw@db.example.com:27017/app".to_string(),
        )])
    }

    #[test]
    fn builds_argv_with_config_file_and_out_dir() {
        let staging = Path::new("/staging/m");
        let inv = mongodump_invocation(&serde_json::json!({}), &secrets(), staging).unwrap();
        let expected_config = staging.join(".mongodump-config.yml").display().to_string();
        let expected_out = staging.join("dump").display().to_string();
        assert_eq!(
            inv.argv,
            vec![
                "--config".to_string(),
                expected_config,
                "--out".to_string(),
                expected_out
            ]
        );
        assert_eq!(
            inv.config_contents,
            "uri: mongodb://user:pw@db.example.com:27017/app\n"
        );
        assert_eq!(inv.config_path, staging.join(".mongodump-config.yml"));
    }

    #[test]
    fn db_setting_appends_db_flag() {
        let inv = mongodump_invocation(
            &serde_json::json!({"db": "app"}),
            &secrets(),
            Path::new("/s"),
        )
        .unwrap();
        assert!(inv.argv.windows(2).any(|w| w == ["--db", "app"]));
    }

    #[test]
    fn uri_never_in_argv() {
        let inv =
            mongodump_invocation(&serde_json::json!({}), &secrets(), Path::new("/s")).unwrap();
        assert!(!inv.argv.iter().any(|a| a.contains("mongodb://")));
    }

    #[test]
    fn missing_uri_names_the_key() {
        // match instead of unwrap_err: unwrap_err requires the Ok type to impl
        // Debug, and MongoInvocation deliberately does not (config_contents
        // carries the secret uri, so a Debug impl would risk leaking it).
        let err =
            match mongodump_invocation(&serde_json::json!({}), &HashMap::new(), Path::new("/s")) {
                Ok(_) => panic!("expected missing 'uri' to be an error"),
                Err(e) => e,
            };
        assert!(err.to_string().contains("uri"));
    }
}
