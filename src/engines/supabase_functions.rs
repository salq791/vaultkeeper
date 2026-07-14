use super::{DumpCtx, Engine, RestoreCtx, VerifyCtx};
use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::process::Command;

pub struct SupabaseFunctionsEngine;

pub fn functions_download_invocation(project_ref: &str) -> Vec<String> {
    vec![
        "functions".to_string(),
        "download".to_string(),
        "--use-api".to_string(),
        "--project-ref".to_string(),
        project_ref.to_string(),
    ]
}

pub fn auth_config_url(api_base: &str, project_ref: &str) -> String {
    format!(
        "{}/v1/projects/{}/config/auth",
        api_base.trim_end_matches('/'),
        project_ref
    )
}

impl Engine for SupabaseFunctionsEngine {
    fn dump(&self, ctx: &DumpCtx) -> Result<PathBuf> {
        let project_ref = ctx
            .settings
            .get("project_ref")
            .and_then(|v| v.as_str())
            .context("supabase_functions settings missing 'project_ref'")?;
        let token = ctx
            .secrets
            .get("access_token")
            .context("supabase_functions secrets missing 'access_token'")?;
        let api_base = ctx
            .settings
            .get("api_base")
            .and_then(|v| v.as_str())
            .unwrap_or("https://api.supabase.com");

        let mut cmd = Command::new("supabase");
        cmd.args(functions_download_invocation(project_ref))
            .current_dir(&ctx.staging_dir)
            .env("SUPABASE_ACCESS_TOKEN", token);
        super::scrub_child_env(&mut cmd);
        let out =
            crate::util::output_with_timeout(&mut cmd, super::timeout_from_settings(&ctx.settings))
                .context("failed to spawn supabase CLI (is it installed?)")?;
        if !out.status.success() {
            bail!(
                "supabase functions download failed: {}",
                crate::util::truncate_marked(&String::from_utf8_lossy(&out.stderr), 2000)
            );
        }

        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context("failed to build http client")?;
        let resp = client
            .get(auth_config_url(api_base, project_ref))
            .bearer_auth(token)
            .send()
            .context("auth config request failed")?;
        if !resp.status().is_success() {
            bail!("auth config request returned HTTP {}", resp.status());
        }
        let body = resp.bytes().context("failed to read auth config body")?;
        // staging_dir is wiped fresh by the pipeline each run, so create_new cannot collide
        // the auth config export can contain SMTP and OAuth provider secrets, hence 0600
        crate::util::write_new_0600(&ctx.staging_dir.join("auth-config.json"), &body)?;
        Ok(ctx.staging_dir.clone())
    }

    fn restore(&self, ctx: &RestoreCtx) -> Result<()> {
        let payload = crate::util::find_named(&ctx.restored_dir, &ctx.source_name)?;
        println!(
            "Edge Functions are redeployed with the supabase CLI, not written back by vaultkeeper."
        );
        println!("Restored source is at: {}", payload.display());
        println!("Steps:");
        println!("  1. cd into the restored directory shown above");
        println!(
            "  2. supabase functions deploy --project-ref <your-project-ref> (per function or all)"
        );
        println!("  3. auth-config.json in the same directory is a reference for manual settings re-entry");
        Ok(())
    }

    fn verify(&self, ctx: &VerifyCtx) -> Result<String> {
        let payload = crate::util::find_named(&ctx.restored_dir, &ctx.source_name)?;
        let fns_dir = payload.join("supabase").join("functions");
        // Count only directory entries: each function is a subdirectory, and
        // stray files alongside them (e.g. import_map.json) must not inflate
        // the count.
        let count = std::fs::read_dir(&fns_dir)
            .with_context(|| {
                format!(
                    "no functions directory in restored snapshot at {}",
                    fns_dir.display()
                )
            })?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .count();
        anyhow::ensure!(count > 0, "verify found zero functions");
        anyhow::ensure!(
            payload.join("auth-config.json").exists(),
            "auth-config.json missing from snapshot"
        );
        Ok(format!("functions={count} auth_config=present"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_invocation_is_exact() {
        assert_eq!(
            functions_download_invocation("abcdefghij1234567890"),
            vec![
                "functions",
                "download",
                "--use-api",
                "--project-ref",
                "abcdefghij1234567890"
            ]
        );
    }

    #[test]
    fn auth_url_builds_and_trims_trailing_slash() {
        assert_eq!(
            auth_config_url("https://api.supabase.com", "ref123"),
            "https://api.supabase.com/v1/projects/ref123/config/auth"
        );
        assert_eq!(
            auth_config_url("https://api.supabase.com/", "ref123"),
            "https://api.supabase.com/v1/projects/ref123/config/auth"
        );
    }

    #[test]
    fn verify_checks_functions_and_auth_config() {
        let d = tempfile::tempdir().unwrap();
        let payload = d.path().join("acme-fns");
        std::fs::create_dir_all(payload.join("supabase").join("functions").join("hello")).unwrap();
        std::fs::write(payload.join("auth-config.json"), b"{}").unwrap();
        let ctx = super::super::VerifyCtx {
            restored_dir: d.path().to_path_buf(),
            source_name: "acme-fns".into(),
            scratch_postgres: None,
            scratch_mongodb: None,
            settings: serde_json::json!({}),
            secrets: std::collections::HashMap::new(),
        };
        let detail = SupabaseFunctionsEngine.verify(&ctx).unwrap();
        assert!(detail.contains("functions=1"));
        assert!(detail.contains("auth_config=present"));
    }

    #[test]
    fn verify_ignores_stray_files_when_counting_functions() {
        let d = tempfile::tempdir().unwrap();
        let payload = d.path().join("acme-fns");
        std::fs::create_dir_all(payload.join("supabase").join("functions").join("hello")).unwrap();
        // A stray file alongside the function directories (e.g. import_map.json)
        // must not inflate the function count.
        std::fs::write(
            payload
                .join("supabase")
                .join("functions")
                .join("import_map.json"),
            b"{}",
        )
        .unwrap();
        std::fs::write(payload.join("auth-config.json"), b"{}").unwrap();
        let ctx = super::super::VerifyCtx {
            restored_dir: d.path().to_path_buf(),
            source_name: "acme-fns".into(),
            scratch_postgres: None,
            scratch_mongodb: None,
            settings: serde_json::json!({}),
            secrets: std::collections::HashMap::new(),
        };
        let detail = SupabaseFunctionsEngine.verify(&ctx).unwrap();
        assert!(
            detail.contains("functions=1"),
            "stray file must not be counted as a function: {detail}"
        );
    }
}
