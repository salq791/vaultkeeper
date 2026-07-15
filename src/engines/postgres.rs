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

/// Normalized host and port for target/scratch safety comparisons.
pub fn url_endpoint(url: &str) -> Option<(String, u16)> {
    let parsed = url::Url::parse(url).ok()?;
    let port = parsed.port().unwrap_or(5432);
    Some((parsed.host_str()?.to_ascii_lowercase(), port))
}

fn source_endpoint(settings: &serde_json::Value) -> Option<(String, u16)> {
    let host = settings.get("host")?.as_str()?.to_ascii_lowercase();
    let port = match settings.get("port") {
        Some(serde_json::Value::Number(value)) => value.as_u64()?,
        Some(serde_json::Value::String(value)) => value.parse().ok()?,
        None => 5432,
        _ => return None,
    };
    u16::try_from(port).ok().map(|port| (host, port))
}

fn matches_source_endpoint(url: &str, settings: &serde_json::Value) -> bool {
    matches!(
        (url_endpoint(url), source_endpoint(settings)),
        (Some(target), Some(source)) if target == source
    )
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
    anyhow::ensure!(
        matches!(u.scheme(), "postgres" | "postgresql"),
        "target url must use postgres:// or postgresql://"
    );
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
        // Keep destructive cleanup and recreation atomic. This also enables
        // pg_restore's exit-on-error behavior.
        "--single-transaction".into(),
        // Dumps carry OWNER TO and GRANT statements referencing roles that do
        // not exist on the target cluster; without these flags pg_restore
        // exits 1 on cross-cluster restores.
        "--no-owner".into(),
        "--no-acl".into(),
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
    let get_string = |key: &str| -> Result<String> {
        let value = settings
            .get(key)
            .and_then(serde_json::Value::as_str)
            .with_context(|| format!("postgres setting '{key}' must be a string"))?;
        anyhow::ensure!(
            !value.is_empty(),
            "postgres setting '{key}' cannot be empty"
        );
        Ok(value.to_string())
    };
    let port = match settings.get("port") {
        Some(serde_json::Value::Number(number)) => number
            .as_u64()
            .context("postgres setting 'port' must be a positive integer")?,
        Some(serde_json::Value::String(value)) => value
            .parse::<u64>()
            .context("postgres setting 'port' must be a positive integer")?,
        _ => bail!("postgres settings missing 'port'"),
    };
    let port =
        u16::try_from(port).context("postgres setting 'port' must be between 1 and 65535")?;
    anyhow::ensure!(
        port > 0,
        "postgres setting 'port' must be between 1 and 65535"
    );
    let password = secrets
        .get("password")
        .context("postgres secrets missing 'password'")?
        .clone();
    anyhow::ensure!(!password.is_empty(), "postgres password cannot be empty");

    let argv = vec![
        "-h".into(),
        get_string("host")?,
        "-p".into(),
        port.to_string(),
        "-U".into(),
        get_string("user")?,
        "-Fc".into(),
        "--compress=0".into(),
        "-f".into(),
        out_file.display().to_string(),
        get_string("dbname")?,
    ];
    let mut env = vec![("PGPASSWORD".to_string(), password)];
    if let Some(ssl) = settings.get("sslmode") {
        let ssl = ssl
            .as_str()
            .context("postgres setting 'sslmode' must be a string")?;
        anyhow::ensure!(
            !ssl.is_empty(),
            "postgres setting 'sslmode' cannot be empty"
        );
        env.push(("PGSSLMODE".to_string(), ssl.to_string()));
    }
    Ok((argv, env))
}

impl Engine for PostgresEngine {
    fn dump(&self, ctx: &DumpCtx) -> Result<std::path::PathBuf> {
        let out_file = ctx.staging_dir.join("db.dump");
        let (argv, env) = pg_dump_invocation(&ctx.settings, &ctx.secrets, &out_file)?;
        let mut cmd = Command::new("pg_dump");
        cmd.args(&argv).envs(env);
        super::scrub_child_env(&mut cmd);
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
        super::require_database_restore_confirmation(ctx)?;
        let target = ctx
            .target
            .as_deref()
            .context("postgres restore requires VAULTKEEPER_RESTORE_TARGET")?;
        if !ctx.force_same_host && matches_source_endpoint(target, &ctx.settings) {
            bail!("target host matches the source host; pass --force-same-host to override");
        }
        let payload = crate::util::find_named(&ctx.restored_dir, &ctx.source_name)?;
        let dump_file = payload.join("db.dump");
        let (argv, env) = pg_restore_invocation(target, &dump_file)?;
        let mut cmd = Command::new("pg_restore");
        cmd.args(&argv).envs(env);
        super::scrub_child_env(&mut cmd);
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
        if matches_source_endpoint(scratch, &ctx.settings) {
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
            .envs(env.clone());
            super::scrub_child_env(&mut cmd);
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
        cmd.args(&argv).envs(env.clone());
        super::scrub_child_env(&mut cmd);
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
                "--single-transaction",
                "--no-owner",
                "--no-acl",
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
    fn url_endpoint_normalizes_host_and_default_port() {
        assert_eq!(
            url_endpoint("postgres://u:p@DB.EXAMPLE.COM/x"),
            Some(("db.example.com".to_string(), 5432))
        );
    }

    #[test]
    fn source_endpoint_accepts_string_port_without_none_equality() {
        let settings = serde_json::json!({"host":"DB.EXAMPLE.COM", "port":"5432"});
        assert!(matches_source_endpoint(
            "postgres://u:p@db.example.com/app",
            &settings
        ));
        assert!(!matches_source_endpoint(
            "not-a-url",
            &serde_json::json!({})
        ));
    }

    #[test]
    fn restore_refuses_same_host_without_force() {
        let ctx = super::super::RestoreCtx {
            restored_dir: std::path::PathBuf::from("/nonexistent"),
            durable_output_dir: std::path::PathBuf::from("/nonexistent-output"),
            secret_temp_dir: std::path::PathBuf::from("/run/vaultkeeper"),
            source_name: "acme-db".into(),
            target: Some("postgres://u:p@db.example.com:5432/other".into()),
            force_same_host: false,
            confirm_remote_overwrite: false,
            confirmed_source: Some("acme-db".into()),
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
            secret_temp_dir: std::path::PathBuf::from("/run/vaultkeeper"),
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
        assert!(argv.contains(&"--no-owner".to_string()));
        assert!(argv.contains(&"--no-acl".to_string()));
        assert!(argv.contains(&"--single-transaction".to_string()));
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
        assert!(argv.contains(&"--no-owner".to_string()));
        assert!(argv.contains(&"--no-acl".to_string()));
        assert!(!argv.iter().any(|a| a.contains("%2D") || a.contains("%2d")));
    }

    #[test]
    fn verify_requires_scratch_postgres() {
        let ctx = super::super::VerifyCtx {
            restored_dir: std::path::PathBuf::from("/nonexistent"),
            secret_temp_dir: std::path::PathBuf::from("/run/vaultkeeper"),
            source_name: "acme-db".into(),
            scratch_postgres: None,
            scratch_mongodb: None,
            settings: serde_json::json!({}),
            secrets: HashMap::new(),
        };
        let err = PostgresEngine.verify(&ctx).unwrap_err();
        assert!(err.to_string().contains("[verify]"));
    }

    #[test]
    fn restore_requires_exact_typed_source_confirmation() {
        let ctx = super::super::RestoreCtx {
            restored_dir: std::path::PathBuf::from("/nonexistent"),
            durable_output_dir: std::path::PathBuf::from("/nonexistent-output"),
            secret_temp_dir: std::path::PathBuf::from("/run/vaultkeeper"),
            source_name: "acme-db".into(),
            target: Some("postgres://u:p@other.example.com:5432/restore".into()),
            force_same_host: false,
            confirm_remote_overwrite: false,
            confirmed_source: Some("wrong-db".into()),
            settings: settings(),
            secrets: HashMap::new(),
        };
        let err = PostgresEngine.restore(&ctx).unwrap_err();
        assert!(err.to_string().contains("--confirm-source acme-db"));
    }
}
