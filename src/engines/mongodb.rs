use super::{DumpCtx, Engine, RestoreCtx, VerifyCtx};
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct MongodbEngine;

/// Naive host extraction for same-host comparison: text between '@' and the
/// next ':' or '/' or end. Handles both mongodb:// and mongodb+srv:// forms.
pub fn uri_host(uri: &str) -> Option<String> {
    let after_at = uri.split('@').nth(1)?;
    let end = after_at.find([':', '/']).unwrap_or(after_at.len());
    Some(after_at[..end].to_string())
}

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

/// Parse mongorestore's "N document(s) restored successfully" line and
/// return N, the restored document count, or None if the marker is absent.
pub fn parse_restored_docs(out: &str) -> Option<u64> {
    for line in out.lines() {
        if let Some(idx) = line.find(" document(s) restored successfully") {
            let head = &line[..idx];
            let num = head.rsplit(|c: char| !c.is_ascii_digit()).next()?;
            let num = head[head.len() - num.len()..].parse().ok()?;
            return Some(num);
        }
    }
    None
}

impl Engine for MongodbEngine {
    fn dump(&self, ctx: &DumpCtx) -> Result<PathBuf> {
        let inv = mongodump_invocation(&ctx.settings, &ctx.secrets, &ctx.staging_dir)?;
        // staging_dir is wiped fresh by the pipeline each run, so create_new cannot collide
        crate::util::write_new_0600(&inv.config_path, inv.config_contents.as_bytes())?;
        let mut cmd = Command::new("mongodump");
        cmd.args(&inv.argv)
            .env_remove("VAULTKEEPER_MASTER_KEY")
            .env_remove("RESTIC_PASSWORD");
        let out =
            crate::util::output_with_timeout(&mut cmd, super::timeout_from_settings(&ctx.settings));
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

    fn restore(&self, ctx: &RestoreCtx) -> Result<()> {
        let target = ctx
            .target
            .as_deref()
            .context("mongodb restore requires --target <mongodb-uri>")?;
        let source_host = ctx
            .secrets
            .get("uri")
            .and_then(|u| uri_host(u))
            .unwrap_or_default();
        if !ctx.force_same_host
            && !source_host.is_empty()
            && uri_host(target).as_deref() == Some(source_host.as_str())
        {
            bail!("target host matches the source host; pass --force-same-host to override");
        }
        let payload = crate::util::find_named(&ctx.restored_dir, &ctx.source_name)?;
        let dump_dir = payload.join("dump");
        let config_path = payload.join(".mongorestore-config.yml");
        crate::util::write_new_0600(&config_path, format!("uri: {target}\n").as_bytes())?;
        let mut cmd = Command::new("mongorestore");
        cmd.args([
            "--config".to_string(),
            config_path.display().to_string(),
            "--drop".to_string(),
            "--dir".to_string(),
            dump_dir.display().to_string(),
        ])
        .env_remove("VAULTKEEPER_MASTER_KEY")
        .env_remove("RESTIC_PASSWORD");
        let out =
            crate::util::output_with_timeout(&mut cmd, super::timeout_from_settings(&ctx.settings));
        let _ = std::fs::remove_file(&config_path);
        if config_path.exists() {
            bail!("mongorestore config file could not be removed; aborting so credentials are not left on disk");
        }
        let out =
            out.context("failed to spawn mongorestore (is mongodb-database-tools installed?)")?;
        if !out.status.success() {
            bail!(
                "mongorestore failed: {}",
                crate::util::truncate_marked(&String::from_utf8_lossy(&out.stderr), 2000)
            );
        }
        Ok(())
    }

    fn verify(&self, ctx: &VerifyCtx) -> Result<String> {
        let scratch = ctx
            .scratch_mongodb
            .as_deref()
            .context("mongodb verify needs a scratch database: configure [verify] mongodb_uri")?;
        let payload = crate::util::find_named(&ctx.restored_dir, &ctx.source_name)?;
        let config_path = payload.join(".mongorestore-config.yml");
        crate::util::write_new_0600(&config_path, format!("uri: {scratch}\n").as_bytes())?;
        let mut cmd = Command::new("mongorestore");
        cmd.args([
            "--config".to_string(),
            config_path.display().to_string(),
            "--drop".to_string(),
            "--dir".to_string(),
            payload.join("dump").display().to_string(),
        ])
        .env_remove("VAULTKEEPER_MASTER_KEY")
        .env_remove("RESTIC_PASSWORD");
        let out =
            crate::util::output_with_timeout(&mut cmd, super::timeout_from_settings(&ctx.settings));
        let _ = std::fs::remove_file(&config_path);
        if config_path.exists() {
            bail!("mongorestore config file could not be removed; aborting so credentials are not left on disk");
        }
        let out = out?;
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if !out.status.success() {
            bail!(
                "verify mongorestore failed: {}",
                crate::util::truncate_marked(&combined, 2000)
            );
        }
        let docs =
            parse_restored_docs(&combined).context("could not parse restored document count")?;
        anyhow::ensure!(docs > 0, "verify restored zero documents");
        Ok(format!("docs={docs}"))
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

    #[test]
    fn uri_host_extracts() {
        assert_eq!(
            uri_host("mongodb://u:p@mongo.example.com:27017/app").as_deref(),
            Some("mongo.example.com")
        );
        assert_eq!(
            uri_host("mongodb+srv://u:p@cluster.example.com/app").as_deref(),
            Some("cluster.example.com")
        );
    }

    #[test]
    fn restore_refuses_same_host_without_force() {
        let ctx = super::super::RestoreCtx {
            restored_dir: std::path::PathBuf::from("/nonexistent"),
            source_name: "acme-db".into(),
            target: Some("mongodb://u:p@mongo.example.com:27017/other".into()),
            force_same_host: false,
            confirm_remote_overwrite: false,
            settings: serde_json::json!({}),
            secrets: HashMap::from([(
                "uri".to_string(),
                "mongodb://u:p@mongo.example.com:27017/app".to_string(),
            )]),
        };
        let err = MongodbEngine.restore(&ctx).unwrap_err();
        assert!(err.to_string().contains("force-same-host"));
    }

    #[test]
    fn parses_mongorestore_doc_count() {
        let out = "2026-07-14T02:00:01.000+0000\t55 document(s) restored successfully. 0 document(s) failed to restore.";
        assert_eq!(parse_restored_docs(out), Some(55));
        assert_eq!(parse_restored_docs("no numbers here"), None);
    }

    #[test]
    fn verify_requires_scratch_mongodb() {
        let ctx = super::super::VerifyCtx {
            restored_dir: std::path::PathBuf::from("/nonexistent"),
            source_name: "acme-db".into(),
            scratch_postgres: None,
            scratch_mongodb: None,
            settings: serde_json::json!({}),
            secrets: HashMap::new(),
        };
        let err = MongodbEngine.verify(&ctx).unwrap_err();
        assert!(err.to_string().contains("[verify]"));
    }
}
