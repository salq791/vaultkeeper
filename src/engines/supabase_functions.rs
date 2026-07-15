use super::{DumpCtx, Engine, RestoreCtx, VerifyCtx};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::path::Path;
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

#[derive(Serialize)]
struct SupplementalManifest {
    captured_config_files: usize,
    source: &'static str,
}

fn is_function_config(name: &str) -> bool {
    matches!(
        name,
        "deno.json" | "deno.jsonc" | "import_map.json" | "import_map.jsonc"
    )
}

fn count_function_configs(root: &Path) -> Result<usize> {
    if !root.is_dir() {
        return Ok(0);
    }
    let mut count = 0usize;
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() && is_function_config(&entry.file_name().to_string_lossy()) {
                count = count.saturating_add(1);
            }
        }
    }
    Ok(count)
}

/// Supabase's download API does not return import maps or deno configuration.
/// Capture those files from a read-only local function source mount while
/// deliberately ignoring code, .env files, dependencies, and symlinks.
fn capture_supplemental_configs(local_functions_dir: &Path, staging: &Path) -> Result<usize> {
    anyhow::ensure!(
        local_functions_dir.is_dir(),
        "supabase_functions local_functions_dir is not a directory: {}",
        local_functions_dir.display()
    );
    // Merge the missing configuration into the same tree operators will
    // deploy. create_new below refuses an unexpected API/local collision
    // instead of silently choosing one version.
    let destination = staging.join("supabase").join("functions");
    crate::util::ensure_private_dir(&destination)?;
    let mut stack = vec![(local_functions_dir.to_path_buf(), PathBuf::new())];
    let mut count = 0usize;
    while let Some((directory, relative)) = stack.pop() {
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let name = entry.file_name();
            let name_text = name.to_string_lossy();
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                if matches!(
                    name_text.as_ref(),
                    ".git" | "node_modules" | ".cache" | "dist"
                ) {
                    continue;
                }
                let child_relative = relative.join(&name);
                stack.push((entry.path(), child_relative));
            } else if kind.is_file() && is_function_config(&name_text) {
                let relative_file = relative.join(&name);
                let target = destination.join(&relative_file);
                if let Some(parent) = target.parent() {
                    crate::util::ensure_private_dir(parent)?;
                }
                crate::util::write_new_0600(&target, &std::fs::read(entry.path())?)?;
                count += 1;
            }
        }
    }
    let manifest = SupplementalManifest {
        captured_config_files: count,
        source: "local_functions_dir",
    };
    crate::util::write_new_0600(
        &staging.join("supplemental-config-manifest.json"),
        &serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(count)
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
        let local_functions_dir = ctx
            .settings
            .get("local_functions_dir")
            .and_then(serde_json::Value::as_str)
            .context(
                "supabase_functions settings missing 'local_functions_dir'; mount the local Supabase functions directory read-only so import maps and deno config are included",
            )?;

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
        capture_supplemental_configs(Path::new(local_functions_dir), &ctx.staging_dir)?;
        Ok(ctx.staging_dir.clone())
    }

    fn restore(&self, ctx: &RestoreCtx) -> Result<()> {
        let payload = crate::util::find_named(&ctx.restored_dir, &ctx.source_name)?;
        let (files, bytes) = crate::util::copy_tree_new(&payload, &ctx.durable_output_dir)?;
        println!(
            "Edge Functions are redeployed with the supabase CLI, not written back by vaultkeeper."
        );
        println!(
            "Restored {files} files ({bytes} bytes) to durable path: {}",
            ctx.durable_output_dir.display()
        );
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
        let manifest_path = payload.join("supplemental-config-manifest.json");
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).with_context(|| {
                format!(
                    "supplemental function configuration manifest missing from snapshot at {}",
                    manifest_path.display()
                )
            })?)
            .context("invalid supplemental function configuration manifest")?;
        let expected_configs = manifest
            .get("captured_config_files")
            .and_then(serde_json::Value::as_u64)
            .context("supplemental manifest missing captured_config_files")?;
        let actual_configs = count_function_configs(&fns_dir)? as u64;
        anyhow::ensure!(
            actual_configs >= expected_configs,
            "snapshot has {actual_configs} function config file(s), but manifest records {expected_configs}"
        );
        Ok(format!(
            "functions={count} auth_config=present supplemental_configs={actual_configs}"
        ))
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
        std::fs::write(
            payload.join("supplemental-config-manifest.json"),
            b"{\"captured_config_files\":0}",
        )
        .unwrap();
        let ctx = super::super::VerifyCtx {
            restored_dir: d.path().to_path_buf(),
            secret_temp_dir: d.path().join("secrets"),
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
        std::fs::write(
            payload.join("supplemental-config-manifest.json"),
            b"{\"captured_config_files\":1}",
        )
        .unwrap();
        let ctx = super::super::VerifyCtx {
            restored_dir: d.path().to_path_buf(),
            secret_temp_dir: d.path().join("secrets"),
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

    #[test]
    fn verify_rejects_missing_supplemental_config_recorded_by_manifest() {
        let d = tempfile::tempdir().unwrap();
        let payload = d.path().join("acme-fns");
        std::fs::create_dir_all(payload.join("supabase/functions/hello")).unwrap();
        std::fs::write(payload.join("auth-config.json"), b"{}").unwrap();
        std::fs::write(
            payload.join("supplemental-config-manifest.json"),
            b"{\"captured_config_files\":1}",
        )
        .unwrap();
        let ctx = super::super::VerifyCtx {
            restored_dir: d.path().to_path_buf(),
            secret_temp_dir: d.path().join("secrets"),
            source_name: "acme-fns".into(),
            scratch_postgres: None,
            scratch_mongodb: None,
            settings: serde_json::json!({}),
            secrets: std::collections::HashMap::new(),
        };
        let error = SupabaseFunctionsEngine.verify(&ctx).unwrap_err();
        assert!(error.to_string().contains("manifest records 1"));
    }

    #[test]
    fn restore_copies_payload_to_durable_output() {
        let d = tempfile::tempdir().unwrap();
        let payload = d.path().join("restic").join("acme-fns");
        std::fs::create_dir_all(payload.join("supabase").join("functions").join("hello")).unwrap();
        std::fs::write(
            payload
                .join("supabase")
                .join("functions")
                .join("hello")
                .join("index.ts"),
            b"serve(() => new Response('ok'))",
        )
        .unwrap();
        let durable = d.path().join("durable").join("snapshot-1");
        let ctx = super::super::RestoreCtx {
            restored_dir: d.path().join("restic"),
            durable_output_dir: durable.clone(),
            secret_temp_dir: d.path().join("secrets"),
            source_name: "acme-fns".into(),
            target: None,
            force_same_host: false,
            confirm_remote_overwrite: false,
            confirmed_source: None,
            settings: serde_json::json!({}),
            secrets: std::collections::HashMap::new(),
        };
        SupabaseFunctionsEngine.restore(&ctx).unwrap();
        assert!(durable.join("supabase/functions/hello/index.ts").is_file());
    }

    #[test]
    fn supplemental_capture_only_copies_dependency_config() {
        let d = tempfile::tempdir().unwrap();
        let local = d.path().join("functions");
        let staging = d.path().join("staging");
        std::fs::create_dir_all(local.join("hello")).unwrap();
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(local.join("hello").join("deno.json"), b"{}").unwrap();
        std::fs::write(local.join("hello").join(".env"), b"SECRET=value").unwrap();
        assert_eq!(capture_supplemental_configs(&local, &staging).unwrap(), 1);
        assert!(staging.join("supabase/functions/hello/deno.json").is_file());
        assert!(!staging.join("supabase/functions/hello/.env").exists());
    }
}
