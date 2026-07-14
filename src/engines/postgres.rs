use super::{DumpCtx, Engine};
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

pub struct PostgresEngine;

type PgDumpInvocation = (Vec<String>, Vec<(String, String)>);

pub fn pg_dump_invocation(
    settings: &serde_json::Value,
    secrets: &HashMap<String, String>,
    out_file: &Path,
) -> Result<PgDumpInvocation> {
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
        let out = Command::new("pg_dump")
            .args(&argv)
            .envs(env)
            .env_remove("VAULTKEEPER_MASTER_KEY")
            .env_remove("RESTIC_PASSWORD")
            .output()
            .context("failed to spawn pg_dump (is it installed and on PATH?)")?;
        if !out.status.success() {
            bail!(
                "pg_dump failed: {}",
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
}
