use super::{DumpCtx, Engine, RestoreCtx, VerifyCtx};
use anyhow::{bail, Context, Result};
use percent_encoding::percent_decode_str;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

pub struct PostgresEngine;

/// Shared (argv, env) shape returned by both pg_dump_invocation and
/// pg_restore_invocation; factored out per clippy::type_complexity.
type PgInvocation = (Vec<String>, Vec<(String, String)>);

/// Host extraction for same-host comparison, via url::Url::parse.
pub fn url_host(url: &str) -> Option<String> {
    url::Url::parse(url).ok()?.host_str().map(|h| h.to_string())
}

/// Fields extracted from a postgres connection URL, with username, password,
/// and dbname percent-decoded (url::Url leaves percent-encoding intact on
/// these components) and an optional sslmode query parameter.
pub(crate) struct PgParts {
    pub host: String,
    pub port: String,
    pub user: String,
    pub password: String,
    pub dbname: String,
    pub sslmode: Option<String>,
}

/// Parse a postgres connection URL into its component parts. Username,
/// password, and the dbname path segment are percent-decoded so that
/// credentials/db names containing reserved URL characters (e.g. `@`, `/`)
/// round-trip correctly instead of being passed to pg_restore/psql still
/// percent-encoded.
pub(crate) fn parse_pg_url(url: &str) -> Result<PgParts> {
    let u = url::Url::parse(url).context("invalid target url")?;
    let host = u.host_str().context("target url missing host")?.to_string();
    let port = u.port().unwrap_or(5432).to_string();
    let user = (!u.username().is_empty())
        .then(|| u.username())
        .context("target url missing user")?;
    let user = percent_decode_str(user).decode_utf8_lossy().to_string();
    let password = u.password().context("target url missing password")?;
    let password = percent_decode_str(password).decode_utf8_lossy().to_string();
    let dbname_raw = u.path().trim_start_matches('/');
    let dbname = percent_decode_str(dbname_raw)
        .decode_utf8_lossy()
        .to_string();
    anyhow::ensure!(!dbname.is_empty(), "target url missing database name");
    let sslmode = u
        .query_pairs()
        .find(|(k, _)| k == "sslmode")
        .map(|(_, v)| v.to_string());
    Ok(PgParts {
        host,
        port,
        user,
        password,
        dbname,
        sslmode,
    })
}

/// Parse `target_url` and build the pg_restore argv + env. The password is
/// delivered ONLY via the returned PGPASSWORD env pair, never argv.
pub fn pg_restore_invocation(target_url: &str, dump_file: &Path) -> Result<PgInvocation> {
    let parts = parse_pg_url(target_url)?;
    let argv = vec![
        "--clean".into(),
        "--if-exists".into(),
        "-h".into(),
        parts.host,
        "-p".into(),
        parts.port,
        "-U".into(),
        parts.user,
        "-d".into(),
        parts.dbname,
        dump_file.display().to_string(),
    ];
    let mut env = vec![("PGPASSWORD".to_string(), parts.password)];
    if let Some(sslmode) = parts.sslmode {
        env.push(("PGSSLMODE".to_string(), sslmode));
    }
    Ok((argv, env))
}

pub fn pg_dump_invocation(
    settings: &serde_json::Value,
    secrets: &HashMap<String, String>,
    out_file: &Path,
) -> Result<PgInvocation> {
    let get = |k: &str| -> Result<String> {
        Ok(settings
            .get(k)
            .with_context(|| format!("postgres settings missing '{k}'"))?
            .to_string()
            .trim_matches('"')
            .to_string())
    };
    let password = secrets
        .get("password")
        .context("postgres secrets missing 'password'")?
        .clone();

    let argv = vec![
        "-h".into(),
        get("host")?,
        "-p".into(),
        get("port")?,
        "-U".into(),
        get("user")?,
        "-Fc".into(),
        "--compress=0".into(),
        "-f".into(),
        out_file.display().to_string(),
        get("dbname")?,
    ];
    let mut env = vec![("PGPASSWORD".to_string(), password)];
    if let Some(ssl) = settings.get("sslmode").and_then(|v| v.as_str()) {
        env.push(("PGSSLMODE".to_string(), ssl.to_string()));
    }
    Ok((argv, env))
}

impl Engine for PostgresEngine {
    fn dump(&self, ctx: &DumpCtx) -> Result<std::path::PathBuf> {
        let out_file = ctx.staging_dir.join("db.dump");
        let (argv, env) = pg_dump_invocation(&ctx.settings, &ctx.secrets, &out_file)?;
        let mut cmd = Command::new("pg_dump");
        cmd.args(&argv)
            .envs(env)
            .env_remove("VAULTKEEPER_MASTER_KEY")
            .env_remove("RESTIC_PASSWORD");
        let out =
            crate::util::output_with_timeout(&mut cmd, super::timeout_from_settings(&ctx.settings))
                .context("failed to spawn pg_dump (is it installed and on PATH?)")?;
        if !out.status.success() {
            bail!(
                "pg_dump failed: {}",
                crate::util::truncate_marked(&String::from_utf8_lossy(&out.stderr), 2000)
            );
        }
        Ok(ctx.staging_dir.clone())
    }

    fn restore(&self, ctx: &RestoreCtx) -> Result<()> {
        let target = ctx
            .target
            .as_deref()
            .context("postgres restore requires --target <postgres-url>")?;
        let source_host = ctx
            .settings
            .get("host")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !ctx.force_same_host
            && url_host(target).is_some_and(|h| h.eq_ignore_ascii_case(source_host))
        {
            bail!("target host matches the source host; pass --force-same-host to override");
        }
        let payload = crate::util::find_named(&ctx.restored_dir, &ctx.source_name)?;
        let dump_file = payload.join("db.dump");
        let (argv, env) = pg_restore_invocation(target, &dump_file)?;
        let mut cmd = Command::new("pg_restore");
        cmd.args(&argv)
            .envs(env)
            .env_remove("VAULTKEEPER_MASTER_KEY")
            .env_remove("RESTIC_PASSWORD");
        let out =
            crate::util::output_with_timeout(&mut cmd, super::timeout_from_settings(&ctx.settings))
                .context("failed to spawn pg_restore (is it installed and on PATH?)")?;
        if !out.status.success() {
            bail!(
                "pg_restore failed: {}",
                crate::util::truncate_marked(&String::from_utf8_lossy(&out.stderr), 2000)
            );
        }
        Ok(())
    }

    fn verify(&self, ctx: &VerifyCtx) -> Result<String> {
        let scratch = ctx
            .scratch_postgres
            .as_deref()
            .context("postgres verify needs a scratch database: configure [verify] postgres_url")?;
        let source_host = ctx
            .settings
            .get("host")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if url_host(scratch).is_some_and(|h| h.eq_ignore_ascii_case(source_host)) {
            bail!("verify scratch host matches the source host; refusing to run verify against the source database");
        }
        let payload = crate::util::find_named(&ctx.restored_dir, &ctx.source_name)?;
        let (argv, env) = pg_restore_invocation(scratch, &payload.join("db.dump"))?;
        let psql = |sql: &str| -> Result<String> {
            let parts = parse_pg_url(scratch)?;
            let mut cmd = Command::new("psql");
            cmd.args([
                "-Atc".to_string(),
                sql.to_string(),
                "-h".to_string(),
                parts.host,
                "-p".to_string(),
                parts.port,
                "-U".to_string(),
                parts.user,
                "-d".to_string(),
                parts.dbname,
            ])
            .envs(env.clone())
            .env_remove("VAULTKEEPER_MASTER_KEY")
            .env_remove("RESTIC_PASSWORD");
            let out = crate::util::output_with_timeout(
                &mut cmd,
                super::timeout_from_settings(&ctx.settings),
            )?;
            anyhow::ensure!(out.status.success(), "psql query failed");
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        };
        // The scratch database is shared and may hold residue from prior
        // verifies; resetting prevents false passes when a snapshot restores
        // zero objects.
        psql("DROP SCHEMA IF EXISTS public CASCADE; CREATE SCHEMA public")?;
        let mut cmd = Command::new("pg_restore");
        cmd.args(&argv)
            .envs(env.clone())
            .env_remove("VAULTKEEPER_MASTER_KEY")
            .env_remove("RESTIC_PASSWORD");
        let out = crate::util::output_with_timeout(
            &mut cmd,
            super::timeout_from_settings(&ctx.settings),
        )?;
        if !out.status.success() {
            bail!(
                "verify pg_restore failed: {}",
                crate::util::truncate_marked(&String::from_utf8_lossy(&out.stderr), 2000)
            );
        }
        psql("ANALYZE")?;
        let tables: i64 =
            psql("SELECT count(*) FROM information_schema.tables WHERE table_schema = 'public'")?
                .parse()?;
        let rows: i64 =
            psql("SELECT coalesce(sum(n_live_tup),0)::bigint FROM pg_stat_user_tables")?.parse()?;
        anyhow::ensure!(tables > 0, "verify restored zero tables");
        Ok(format!("tables={tables} approx_rows={rows}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::Path;

    fn settings() -> serde_json::Value {
        serde_json::json!({"host": "db.example.com", "port": 5432, "dbname": "app", "user": "postgres", "sslmode": "require"})
    }

    #[test]
    fn builds_pg_dump_argv_and_env() {
        let secrets = HashMap::from([("password".to_string(), "pw".to_string())]);
        let (argv, env) =
            pg_dump_invocation(&settings(), &secrets, Path::new("/staging/x/db.dump")).unwrap();
        assert_eq!(
            argv,
            vec![
                "-h",
                "db.example.com",
                "-p",
                "5432",
                "-U",
                "postgres",
                "-Fc",
                "--compress=0",
                "-f",
                "/staging/x/db.dump",
                "app"
            ]
        );
        assert!(env.contains(&("PGPASSWORD".to_string(), "pw".to_string())));
        assert!(env.contains(&("PGSSLMODE".to_string(), "require".to_string())));
    }

    #[test]
    fn missing_password_is_error() {
        let err =
            pg_dump_invocation(&settings(), &HashMap::new(), Path::new("/tmp/x")).unwrap_err();
        assert!(err.to_string().contains("password"));
    }

    #[test]
    fn pg_restore_invocation_keeps_password_in_env() {
        let (argv, env) = pg_restore_invocation(
            "postgres://admin:s3cret@restore.example.com:5433/newdb",
            Path::new("/r/acme-db/db.dump"),
        )
        .unwrap();
        assert_eq!(
            argv,
            vec![
                "--clean",
                "--if-exists",
                "-h",
                "restore.example.com",
                "-p",
                "5433",
                "-U",
                "admin",
                "-d",
                "newdb",
                "/r/acme-db/db.dump"
            ]
        );
        assert!(env.contains(&("PGPASSWORD".to_string(), "s3cret".to_string())));
        assert!(!argv.iter().any(|a| a.contains("s3cret")));
    }

    #[test]
    fn url_host_extracts() {
        assert_eq!(
            url_host("postgres://u:p@db.example.com:5432/x").as_deref(),
            Some("db.example.com")
        );
    }

    #[test]
    fn restore_refuses_same_host_without_force() {
        let ctx = super::super::RestoreCtx {
            restored_dir: std::path::PathBuf::from("/nonexistent"),
            source_name: "acme-db".into(),
            target: Some("postgres://u:p@db.example.com:5432/other".into()),
            force_same_host: false,
            confirm_remote_overwrite: false,
            settings: serde_json::json!({"host": "db.example.com", "port": 5432, "dbname": "app", "user": "u"}),
            secrets: std::collections::HashMap::new(),
        };
        let err = PostgresEngine.restore(&ctx).unwrap_err();
        assert!(err.to_string().contains("force-same-host"));
    }

    #[test]
    fn verify_refuses_scratch_on_source_host() {
        let ctx = super::super::VerifyCtx {
            restored_dir: std::path::PathBuf::from("/nonexistent"),
            source_name: "acme-db".into(),
            scratch_postgres: Some("postgres://u:p@DB.EXAMPLE.COM:5432/scratch".into()),
            scratch_mongodb: None,
            settings: serde_json::json!({"host": "db.example.com", "port": 5432, "dbname": "app", "user": "u"}),
            secrets: HashMap::new(),
        };
        let err = PostgresEngine.verify(&ctx).unwrap_err();
        assert!(err.to_string().contains("refusing"));
    }

    #[test]
    fn pg_restore_invocation_decodes_percent_encoded_credentials_and_honors_sslmode() {
        let (argv, env) = pg_restore_invocation(
            "postgres://admin:p%40ss%2Fword@restore.example.com:5433/newdb?sslmode=require",
            Path::new("/r/acme-db/db.dump"),
        )
        .unwrap();
        assert!(env.contains(&("PGPASSWORD".to_string(), "p@ss/word".to_string())));
        assert!(env.contains(&("PGSSLMODE".to_string(), "require".to_string())));
        assert!(argv.contains(&"newdb".to_string()));
        assert!(!argv
            .iter()
            .any(|a| a.contains("p%40ss%2Fword") || a.contains("p@ss/word")));
    }

    #[test]
    fn pg_restore_invocation_decodes_percent_encoded_dbname() {
        let (argv, _env) = pg_restore_invocation(
            "postgres://admin:pw@restore.example.com:5433/my%2Ddb",
            Path::new("/r/acme-db/db.dump"),
        )
        .unwrap();
        assert!(argv.contains(&"my-db".to_string()));
        assert!(!argv.iter().any(|a| a.contains("%2D") || a.contains("%2d")));
    }

    #[test]
    fn verify_requires_scratch_postgres() {
        let ctx = super::super::VerifyCtx {
            restored_dir: std::path::PathBuf::from("/nonexistent"),
            source_name: "acme-db".into(),
            scratch_postgres: None,
            scratch_mongodb: None,
            settings: serde_json::json!({}),
            secrets: HashMap::new(),
        };
        let err = PostgresEngine.verify(&ctx).unwrap_err();
        assert!(err.to_string().contains("[verify]"));
    }
}
