use super::{DumpCtx, Engine, RestoreCtx};
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

type RcloneInvocation = (Vec<String>, Vec<(String, String)>);

pub struct SupabaseStorageEngine;

pub fn rclone_invocation(
    settings: &serde_json::Value,
    secrets: &HashMap<String, String>,
    mirror_root: &Path,
) -> Result<RcloneInvocation> {
    let endpoint = settings
        .get("endpoint")
        .and_then(|v| v.as_str())
        .context("supabase_storage settings missing 'endpoint'")?;
    let access_key = secrets
        .get("access_key")
        .context("supabase_storage secrets missing 'access_key'")?;
    let secret_key = secrets
        .get("secret_key")
        .context("supabase_storage secrets missing 'secret_key'")?;

    let argv = vec![
        "sync".to_string(),
        "SUPA:".to_string(),
        mirror_root.display().to_string(),
    ];
    let mut env = vec![
        ("RCLONE_CONFIG_SUPA_TYPE".to_string(), "s3".to_string()),
        (
            "RCLONE_CONFIG_SUPA_PROVIDER".to_string(),
            "Other".to_string(),
        ),
        (
            "RCLONE_CONFIG_SUPA_ACCESS_KEY_ID".to_string(),
            access_key.clone(),
        ),
        (
            "RCLONE_CONFIG_SUPA_SECRET_ACCESS_KEY".to_string(),
            secret_key.clone(),
        ),
        (
            "RCLONE_CONFIG_SUPA_ENDPOINT".to_string(),
            endpoint.to_string(),
        ),
    ];
    if let Some(region) = settings.get("region").and_then(|v| v.as_str()) {
        env.push(("RCLONE_CONFIG_SUPA_REGION".to_string(), region.to_string()));
    }
    Ok((argv, env))
}

impl Engine for SupabaseStorageEngine {
    fn dump(&self, ctx: &DumpCtx) -> Result<PathBuf> {
        let (argv, env) = rclone_invocation(&ctx.settings, &ctx.secrets, &ctx.mirror_root)?;
        let mut cmd = Command::new("rclone");
        cmd.args(&argv)
            .envs(env)
            .env_remove("VAULTKEEPER_MASTER_KEY")
            .env_remove("RESTIC_PASSWORD");
        let out =
            crate::util::output_with_timeout(&mut cmd, super::timeout_from_settings(&ctx.settings))
                .context("failed to spawn rclone (is it installed?)")?;
        if !out.status.success() {
            bail!(
                "rclone sync failed: {}",
                crate::util::truncate_marked(&String::from_utf8_lossy(&out.stderr), 2000)
            );
        }
        Ok(ctx.mirror_root.clone())
    }

    fn restore(&self, ctx: &RestoreCtx) -> Result<()> {
        anyhow::ensure!(
            ctx.confirm_remote_overwrite,
            "storage restore OVERWRITES the remote bucket contents; pass --confirm-remote-overwrite to proceed"
        );
        let mirror = crate::util::find_named(&ctx.restored_dir, &ctx.source_name)?;
        // rclone_invocation's argv direction is dump-shaped (remote -> mirror);
        // restore reuses only its env config and builds the reversed argv here.
        let (_, env) = rclone_invocation(&ctx.settings, &ctx.secrets, &mirror)?;
        let mut cmd = Command::new("rclone");
        cmd.args([
            "sync".to_string(),
            mirror.display().to_string(),
            "SUPA:".to_string(),
        ])
        .envs(env)
        .env_remove("VAULTKEEPER_MASTER_KEY")
        .env_remove("RESTIC_PASSWORD");
        let out =
            crate::util::output_with_timeout(&mut cmd, super::timeout_from_settings(&ctx.settings))
                .context("failed to spawn rclone (is it installed?)")?;
        if !out.status.success() {
            bail!(
                "rclone restore sync failed: {}",
                crate::util::truncate_marked(&String::from_utf8_lossy(&out.stderr), 2000)
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::Path;

    fn settings() -> serde_json::Value {
        serde_json::json!({"endpoint": "https://proj.storage.example.com/storage/v1/s3", "region": "us-east-1"})
    }

    fn secrets() -> HashMap<String, String> {
        HashMap::from([
            ("access_key".to_string(), "AK".to_string()),
            ("secret_key".to_string(), "SK".to_string()),
        ])
    }

    #[test]
    fn builds_sync_argv_and_env_config() {
        let (argv, env) =
            rclone_invocation(&settings(), &secrets(), Path::new("/staging/.mirrors/x")).unwrap();
        assert_eq!(argv[0], "sync");
        assert_eq!(argv[1], "SUPA:");
        assert!(argv[2].ends_with("x"));
        assert!(env.contains(&("RCLONE_CONFIG_SUPA_TYPE".into(), "s3".into())));
        assert!(env.contains(&("RCLONE_CONFIG_SUPA_PROVIDER".into(), "Other".into())));
        assert!(env.contains(&("RCLONE_CONFIG_SUPA_ACCESS_KEY_ID".into(), "AK".into())));
        assert!(env.contains(&("RCLONE_CONFIG_SUPA_SECRET_ACCESS_KEY".into(), "SK".into())));
        assert!(env.contains(&(
            "RCLONE_CONFIG_SUPA_ENDPOINT".into(),
            "https://proj.storage.example.com/storage/v1/s3".into()
        )));
        assert!(env.contains(&("RCLONE_CONFIG_SUPA_REGION".into(), "us-east-1".into())));
    }

    #[test]
    fn secrets_never_in_argv() {
        let (argv, _) = rclone_invocation(&settings(), &secrets(), Path::new("/m")).unwrap();
        assert!(!argv.iter().any(|a| a.contains("AK") || a.contains("SK")));
    }

    #[test]
    fn missing_fields_name_the_key() {
        let e1 =
            rclone_invocation(&serde_json::json!({}), &secrets(), Path::new("/m")).unwrap_err();
        assert!(e1.to_string().contains("endpoint"));
        let e2 = rclone_invocation(&settings(), &HashMap::new(), Path::new("/m")).unwrap_err();
        assert!(e2.to_string().contains("access_key"));
    }

    #[test]
    fn storage_restore_requires_confirmation() {
        let ctx = super::super::RestoreCtx {
            restored_dir: std::path::PathBuf::from("/nonexistent"),
            source_name: "acme-storage".into(),
            target: None,
            force_same_host: false,
            confirm_remote_overwrite: false,
            settings: serde_json::json!({"endpoint": "https://proj.storage.example.com/storage/v1/s3"}),
            secrets: std::collections::HashMap::from([
                ("access_key".to_string(), "AK".to_string()),
                ("secret_key".to_string(), "SK".to_string()),
            ]),
        };
        let err = SupabaseStorageEngine.restore(&ctx).unwrap_err();
        assert!(err.to_string().contains("confirm-remote-overwrite"));
    }
}
