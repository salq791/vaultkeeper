use super::{DumpCtx, Engine, RestoreCtx, VerifyCtx};
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct MongodbEngine;

/// Parse every host in a MongoDB URI, including credential-less and
/// replica-set seed-list forms. Host names are normalized case-insensitively
/// and missing ports use MongoDB's default 27017.
pub fn uri_endpoints(uri: &str) -> Result<Vec<(String, u16)>> {
    let authority = uri
        .strip_prefix("mongodb://")
        .or_else(|| uri.strip_prefix("mongodb+srv://"))
        .context("mongodb uri must use mongodb:// or mongodb+srv://")?
        .split(['/', '?', '#'])
        .next()
        .context("mongodb uri missing authority")?;
    let hosts = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    anyhow::ensure!(!hosts.is_empty(), "mongodb uri missing host");

    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut bracket_depth = 0u8;
    for (idx, ch) in hosts.char_indices() {
        match ch {
            '[' => bracket_depth = bracket_depth.saturating_add(1),
            ']' => bracket_depth = bracket_depth.saturating_sub(1),
            ',' if bracket_depth == 0 => {
                parts.push(&hosts[start..idx]);
                start = idx + 1;
            }
            _ => {}
        }
    }
    parts.push(&hosts[start..]);

    parts
        .into_iter()
        .map(|part| {
            anyhow::ensure!(!part.is_empty(), "mongodb uri contains an empty host");
            if let Some(rest) = part.strip_prefix('[') {
                let close = rest.find(']').context("invalid bracketed mongodb host")?;
                let host = rest[..close].to_ascii_lowercase();
                let suffix = &rest[close + 1..];
                let port = if suffix.is_empty() {
                    27017
                } else {
                    suffix
                        .strip_prefix(':')
                        .context("invalid bracketed mongodb port")?
                        .parse::<u16>()
                        .context("invalid mongodb port")?
                };
                return Ok((host, port));
            }
            let (host, port) = match part.rsplit_once(':') {
                Some((host, port)) if !host.contains(':') => {
                    (host, port.parse::<u16>().context("invalid mongodb port")?)
                }
                _ => (part, 27017),
            };
            anyhow::ensure!(!host.is_empty(), "mongodb uri missing host");
            Ok((host.to_ascii_lowercase(), port))
        })
        .collect()
}

fn shares_endpoint(left: &str, right: &str) -> bool {
    let Ok(left) = uri_endpoints(left) else {
        return false;
    };
    let Ok(right) = uri_endpoints(right) else {
        return false;
    };
    left.iter().any(|endpoint| right.contains(endpoint))
}

// No Debug derive: config_contents carries the raw secret uri, and a Debug
// impl would let any future {:?} or dbg!() leak credentials into logs.
pub struct MongoInvocation {
    pub argv: Vec<String>,
}

pub fn mongodump_invocation(
    settings: &serde_json::Value,
    secrets: &HashMap<String, String>,
    staging_dir: &Path,
    config_path: &Path,
) -> Result<MongoInvocation> {
    let uri = secrets
        .get("uri")
        .context("mongodb secrets missing 'uri' (full connection string)")?;
    uri_endpoints(uri)?;
    let out_dir = staging_dir.join("dump");
    let mut argv = vec![
        "--config".to_string(),
        config_path.display().to_string(),
        "--out".to_string(),
        out_dir.display().to_string(),
    ];
    let db = match settings.get("db") {
        Some(value) => {
            let value = value
                .as_str()
                .context("mongodb setting 'db' must be a string")?;
            anyhow::ensure!(!value.is_empty(), "mongodb setting 'db' cannot be empty");
            Some(value)
        }
        None => None,
    };
    if let Some(db) = db {
        argv.push("--db".to_string());
        argv.push(db.to_string());
    }
    let bool_setting = |name: &str| -> Result<bool> {
        settings
            .get(name)
            .map(|value| {
                value
                    .as_bool()
                    .with_context(|| format!("mongodb setting '{name}' must be a boolean"))
            })
            .transpose()
            .map(|value| value.unwrap_or(false))
    };
    let oplog = bool_setting("oplog")?;
    let allow_inconsistent = bool_setting("allow_inconsistent_dump")?;
    anyhow::ensure!(
        oplog || allow_inconsistent,
        "mongodb backup consistency is not configured: set oplog=true for a full replica-set dump, or explicitly set allow_inconsistent_dump=true"
    );
    anyhow::ensure!(
        !(oplog && db.is_some()),
        "mongodb oplog=true cannot be combined with the db setting; oplog backups must dump the full replica set"
    );
    if oplog {
        argv.push("--oplog".to_string());
    }
    Ok(MongoInvocation { argv })
}

fn mongo_config_file(
    secret_temp_dir: &Path,
    prefix: &str,
    uri: &str,
) -> Result<tempfile::NamedTempFile> {
    crate::util::ensure_private_dir(secret_temp_dir)?;
    let mut file = tempfile::Builder::new()
        .prefix(prefix)
        .suffix(".yml")
        .tempfile_in(secret_temp_dir)
        .context("failed to create runtime-only mongodb credential file")?;
    // JSON string syntax is a valid YAML scalar and safely quotes `#`, `:`,
    // backslashes, and other URI characters that have YAML meaning.
    let contents = format!("uri: {}\n", serde_json::to_string(uri)?);
    file.write_all(contents.as_bytes())
        .context("failed to write runtime-only mongodb credential file")?;
    file.flush()
        .context("failed to flush runtime-only mongodb credential file")?;
    Ok(file)
}

fn mongorestore_args(config_path: &Path, dump_dir: &Path, oplog: bool) -> Vec<String> {
    let mut args = vec![
        "--config".to_string(),
        config_path.display().to_string(),
        "--drop".to_string(),
        "--stopOnError".to_string(),
        "--dir".to_string(),
        dump_dir.display().to_string(),
    ];
    if oplog {
        args.push("--oplogReplay".to_string());
    }
    args
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

/// Fails restore when mongorestore reports zero restored documents. A silent
/// zero-document restore reporting success is the worst failure mode for a
/// backup tool; the most common cause is a target uri that names a database,
/// which makes `mongorestore --dir <dump root>` skip the dump's per-database
/// subdirectories.
fn ensure_docs_restored(combined: &str) -> Result<()> {
    match parse_restored_docs(combined) {
        Some(n) if n > 0 => Ok(()),
        _ => anyhow::bail!(
            "mongorestore reported zero restored documents; check that the target uri has no database path"
        ),
    }
}

/// Parse mongorestore's "N document(s) failed to restore" line and return N,
/// or None if the marker is absent.
pub fn parse_failed_docs(out: &str) -> Option<u64> {
    for line in out.lines() {
        if let Some(idx) = line.find(" document(s) failed to restore") {
            let head = &line[..idx];
            let digits: String = head
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            let digits: String = digits.chars().rev().collect();
            return digits.parse().ok();
        }
    }
    None
}

impl Engine for MongodbEngine {
    fn dump(&self, ctx: &DumpCtx) -> Result<PathBuf> {
        let uri = ctx
            .secrets
            .get("uri")
            .context("mongodb secrets missing 'uri' (full connection string)")?;
        let config = mongo_config_file(
            &ctx.secret_temp_dir,
            &format!("mongodump-{}-", ctx.source_name),
            uri,
        )?;
        let inv =
            mongodump_invocation(&ctx.settings, &ctx.secrets, &ctx.staging_dir, config.path())?;
        let mut cmd = Command::new("mongodump");
        cmd.args(&inv.argv);
        super::scrub_child_env(&mut cmd);
        let out =
            crate::util::output_with_timeout(&mut cmd, super::timeout_from_settings(&ctx.settings));
        config
            .close()
            .context("failed to remove runtime-only mongodump credential file")?;
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
        super::require_database_restore_confirmation(ctx)?;
        let target = ctx
            .target
            .as_deref()
            .context("mongodb restore requires VAULTKEEPER_RESTORE_TARGET")?;
        let source_uri = ctx
            .secrets
            .get("uri")
            .context("mongodb secrets missing 'uri' (full connection string)")?;
        if !ctx.force_same_host && shares_endpoint(source_uri, target) {
            bail!("target host matches the source host; pass --force-same-host to override");
        }
        let payload = crate::util::find_named(&ctx.restored_dir, &ctx.source_name)?;
        let dump_dir = payload.join("dump");
        let config = mongo_config_file(
            &ctx.secret_temp_dir,
            &format!("mongorestore-{}-", ctx.source_name),
            target,
        )?;
        let mut cmd = Command::new("mongorestore");
        cmd.args(mongorestore_args(
            config.path(),
            &dump_dir,
            dump_dir.join("oplog.bson").is_file(),
        ));
        super::scrub_child_env(&mut cmd);
        let out =
            crate::util::output_with_timeout(&mut cmd, super::timeout_from_settings(&ctx.settings));
        config
            .close()
            .context("failed to remove runtime-only mongorestore credential file")?;
        let out =
            out.context("failed to spawn mongorestore (is mongodb-database-tools installed?)")?;
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if !out.status.success() {
            bail!(
                "mongorestore failed: {}",
                crate::util::truncate_marked(&combined, 2000)
            );
        }
        ensure_docs_restored(&combined)?;
        if let Some(failed) = parse_failed_docs(&combined) {
            anyhow::ensure!(
                failed == 0,
                "mongorestore reported {failed} failed document(s)"
            );
        }
        Ok(())
    }

    fn verify(&self, ctx: &VerifyCtx) -> Result<String> {
        let scratch = ctx
            .scratch_mongodb
            .as_deref()
            .context("mongodb verify needs a scratch database: configure [verify] mongodb_uri")?;
        let source_uri = ctx
            .secrets
            .get("uri")
            .context("mongodb secrets missing 'uri' (full connection string)")?;
        if shares_endpoint(source_uri, scratch) {
            bail!("verify scratch host matches the source host; refusing to run verify against the source database");
        }
        let payload = crate::util::find_named(&ctx.restored_dir, &ctx.source_name)?;
        let dump_dir = payload.join("dump");
        let config = mongo_config_file(
            &ctx.secret_temp_dir,
            &format!("mongo-verify-{}-", ctx.source_name),
            scratch,
        )?;
        let mut cmd = Command::new("mongorestore");
        cmd.args(mongorestore_args(
            config.path(),
            &dump_dir,
            dump_dir.join("oplog.bson").is_file(),
        ));
        super::scrub_child_env(&mut cmd);
        let out =
            crate::util::output_with_timeout(&mut cmd, super::timeout_from_settings(&ctx.settings));
        config
            .close()
            .context("failed to remove runtime-only mongo verify credential file")?;
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
        // Unlike postgres verify, the scratch database here does not need an
        // explicit reset: --drop above clears collections that this dump
        // touches, and the doc count below comes from THIS run's
        // mongorestore output, not a post-hoc database query, so residue
        // left behind by a prior verify (e.g. extra collections outside this
        // dump) cannot inflate the count and cause a false pass.
        let docs =
            parse_restored_docs(&combined).context("could not parse restored document count")?;
        anyhow::ensure!(docs > 0, "verify restored zero documents");
        if let Some(failed) = parse_failed_docs(&combined) {
            anyhow::ensure!(
                failed == 0,
                "verify mongorestore reported {failed} failed document(s)"
            );
        }
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

    fn best_effort_settings() -> serde_json::Value {
        serde_json::json!({"allow_inconsistent_dump": true})
    }

    #[test]
    fn builds_argv_with_config_file_and_out_dir() {
        let staging = Path::new("/staging/m");
        let config = Path::new("/run/vaultkeeper/mongodb.yml");
        let inv =
            mongodump_invocation(&best_effort_settings(), &secrets(), staging, config).unwrap();
        let expected_config = config.display().to_string();
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
        assert!(!inv.argv.iter().any(|arg| arg.contains("user:pw")));
    }

    #[test]
    fn db_setting_appends_db_flag() {
        let inv = mongodump_invocation(
            &serde_json::json!({"db": "app", "allow_inconsistent_dump": true}),
            &secrets(),
            Path::new("/s"),
            Path::new("/run/mongo.yml"),
        )
        .unwrap();
        assert!(inv.argv.windows(2).any(|w| w == ["--db", "app"]));
    }

    #[test]
    fn uri_never_in_argv() {
        let inv = mongodump_invocation(
            &best_effort_settings(),
            &secrets(),
            Path::new("/s"),
            Path::new("/run/mongo.yml"),
        )
        .unwrap();
        assert!(!inv.argv.iter().any(|a| a.contains("mongodb://")));
    }

    #[test]
    fn runtime_config_quotes_uri_and_is_removable() {
        let directory = tempfile::tempdir().unwrap();
        let config = mongo_config_file(
            directory.path(),
            "mongo-test-",
            "mongodb://user:p%23w@db.example.com/app?x=a:b",
        )
        .unwrap();
        let path = config.path().to_path_buf();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.starts_with("uri: \"mongodb://"));
        assert!(contents.contains("p%23w"));
        config.close().unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn missing_uri_names_the_key() {
        // match instead of unwrap_err: unwrap_err requires the Ok type to impl
        // Debug, and MongoInvocation deliberately does not (config_contents
        // carries the secret uri, so a Debug impl would risk leaking it).
        let err = match mongodump_invocation(
            &best_effort_settings(),
            &HashMap::new(),
            Path::new("/s"),
            Path::new("/run/mongo.yml"),
        ) {
            Ok(_) => panic!("expected missing 'uri' to be an error"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("uri"));
    }

    #[test]
    fn uri_endpoints_handle_credentials_case_and_seed_lists() {
        assert_eq!(
            uri_endpoints("mongodb://u:p@Mongo.Example.com:27018,SECOND:27019/app").unwrap(),
            vec![
                ("mongo.example.com".to_string(), 27018),
                ("second".to_string(), 27019)
            ]
        );
        assert_eq!(
            uri_endpoints("mongodb+srv://cluster.example.com/app").unwrap(),
            vec![("cluster.example.com".to_string(), 27017)]
        );
    }

    #[test]
    fn restore_refuses_same_host_without_force() {
        let ctx = super::super::RestoreCtx {
            restored_dir: std::path::PathBuf::from("/nonexistent"),
            durable_output_dir: std::path::PathBuf::from("/nonexistent-output"),
            secret_temp_dir: std::path::PathBuf::from("/run/vaultkeeper"),
            source_name: "acme-db".into(),
            target: Some("mongodb://u:p@mongo.example.com:27017/other".into()),
            force_same_host: false,
            confirm_remote_overwrite: false,
            confirmed_source: Some("acme-db".into()),
            settings: best_effort_settings(),
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
    fn verify_refuses_scratch_on_source_host() {
        let ctx = super::super::VerifyCtx {
            restored_dir: std::path::PathBuf::from("/nonexistent"),
            secret_temp_dir: std::path::PathBuf::from("/run/vaultkeeper"),
            source_name: "acme-db".into(),
            scratch_postgres: None,
            scratch_mongodb: Some("mongodb://u:p@DB.EXAMPLE.COM:27017/scratch".into()),
            settings: serde_json::json!({}),
            secrets: HashMap::from([(
                "uri".to_string(),
                "mongodb://u:p@db.example.com:27017/app".to_string(),
            )]),
        };
        let err = MongodbEngine.verify(&ctx).unwrap_err();
        assert!(err.to_string().contains("refusing"));
    }

    #[test]
    fn parses_mongorestore_failed_count() {
        let out = "55 document(s) restored successfully. 5 document(s) failed to restore.";
        assert_eq!(parse_failed_docs(out), Some(5));
        assert_eq!(
            parse_failed_docs(
                "55 document(s) restored successfully. 0 document(s) failed to restore."
            ),
            Some(0)
        );
        assert_eq!(parse_failed_docs("nothing here"), None);
    }

    #[test]
    fn restore_fails_on_zero_restored_documents() {
        let out = "0 document(s) restored successfully. 0 document(s) failed to restore.";
        let err = ensure_docs_restored(out).unwrap_err();
        assert!(err.to_string().contains("zero restored documents"));
    }

    #[test]
    fn restore_passes_when_documents_restored() {
        let out = "3 document(s) restored successfully. 0 document(s) failed to restore.";
        assert!(ensure_docs_restored(out).is_ok());
    }

    #[test]
    fn verify_requires_scratch_mongodb() {
        let ctx = super::super::VerifyCtx {
            restored_dir: std::path::PathBuf::from("/nonexistent"),
            secret_temp_dir: std::path::PathBuf::from("/run/vaultkeeper"),
            source_name: "acme-db".into(),
            scratch_postgres: None,
            scratch_mongodb: None,
            settings: serde_json::json!({}),
            secrets: HashMap::new(),
        };
        let err = MongodbEngine.verify(&ctx).unwrap_err();
        assert!(err.to_string().contains("[verify]"));
    }

    #[test]
    fn oplog_mode_is_dumped_and_replayed() {
        let inv = mongodump_invocation(
            &serde_json::json!({"oplog": true}),
            &secrets(),
            Path::new("/staging"),
            Path::new("/run/mongo.yml"),
        )
        .unwrap();
        assert!(inv.argv.contains(&"--oplog".to_string()));
        let restore = mongorestore_args(
            Path::new("/run/mongo.yml"),
            Path::new("/staging/dump"),
            true,
        );
        assert!(restore.contains(&"--oplogReplay".to_string()));
        assert!(restore.contains(&"--stopOnError".to_string()));
    }

    #[test]
    fn consistency_mode_must_be_explicit() {
        let err = match mongodump_invocation(
            &serde_json::json!({}),
            &secrets(),
            Path::new("/staging"),
            Path::new("/run/mongo.yml"),
        ) {
            Ok(_) => panic!("expected consistency mode error"),
            Err(error) => error,
        };
        assert!(err.to_string().contains("consistency"));
    }

    #[test]
    fn oplog_mode_rejects_single_database_filter() {
        let err = match mongodump_invocation(
            &serde_json::json!({"oplog": true, "db": "app"}),
            &secrets(),
            Path::new("/staging"),
            Path::new("/run/mongo.yml"),
        ) {
            Ok(_) => panic!("expected oplog/db incompatibility"),
            Err(error) => error,
        };
        assert!(err.to_string().contains("cannot be combined"));
    }
}
