use super::{DumpCtx, Engine};
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

#[allow(dead_code)]
pub struct MongodbEngine;

#[derive(Debug)]
pub struct MongoInvocation {
    #[allow(dead_code)]
    pub argv: Vec<String>,
    #[allow(dead_code)]
    pub config_path: PathBuf,
    #[allow(dead_code)]
    pub config_contents: String,
}

#[allow(dead_code)]
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
        std::fs::write(&inv.config_path, &inv.config_contents)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&inv.config_path, std::fs::Permissions::from_mode(0o600))?;
        }
        let out = Command::new("mongodump")
            .args(&inv.argv)
            .env_remove("VAULTKEEPER_MASTER_KEY")
            .env_remove("RESTIC_PASSWORD")
            .output();
        let _ = std::fs::remove_file(&inv.config_path);
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
        let err = mongodump_invocation(&serde_json::json!({}), &HashMap::new(), Path::new("/s"))
            .unwrap_err();
        assert!(err.to_string().contains("uri"));
    }
}
