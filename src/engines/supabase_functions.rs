use super::{DumpCtx, Engine};
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

        let out = Command::new("supabase")
            .args(functions_download_invocation(project_ref))
            .current_dir(&ctx.staging_dir)
            .env("SUPABASE_ACCESS_TOKEN", token)
            .env_remove("VAULTKEEPER_MASTER_KEY")
            .env_remove("RESTIC_PASSWORD")
            .output()
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
        {
            use std::io::Write;
            let mut opts = std::fs::OpenOptions::new();
            // staging_dir is wiped fresh by the pipeline each run, so create_new cannot collide
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut f = opts
                .open(ctx.staging_dir.join("auth-config.json"))
                .context("failed to create auth config file")?;
            f.write_all(&body)
                .context("failed to write auth config file")?;
        }
        Ok(ctx.staging_dir.clone())
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
}
