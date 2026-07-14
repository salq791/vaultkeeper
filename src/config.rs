use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
pub struct Config {
    pub global: Global,
    #[serde(default)]
    #[allow(dead_code)]
    pub notify: Notify,
}

#[derive(Debug, Deserialize)]
pub struct Global {
    pub staging_dir: PathBuf,
    pub restic_repo: String,
    pub restic_password: String,
    #[serde(default)]
    pub restic_timeout_minutes: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[allow(dead_code)]
pub struct Notify {
    pub healthchecks_base: Option<String>,
    pub webhook_url: Option<String>,
    pub ses: Option<Ses>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct Ses {
    pub region: String,
    pub from: String,
    pub to: Vec<String>,
}

/// Replace every ${NAME} with lookup(NAME); error naming the var when absent.
fn interpolate(s: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Result<String> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after.find('}').context("unclosed ${ in config")?;
        let name = &after[..end];
        match lookup(name) {
            Some(v) => out.push_str(&v),
            None => bail!("environment variable {name} is not set (referenced in config)"),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

pub fn load_from_str(text: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Result<Config> {
    let interpolated = interpolate(text, lookup)?;
    toml::from_str(&interpolated).context("invalid config.toml")
}

pub fn load(path: &Path) -> Result<Config> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read config file {}", path.display()))?;
    load_from_str(&text, &|k| std::env::var(k).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[global]
staging_dir = "/staging"
restic_repo = "sftp:demo@demo.repo.example.com:vk"
restic_password = "${RESTIC_PASSWORD}"
restic_timeout_minutes = 300

[notify]
healthchecks_base = "https://hc-ping.com"
"#;

    const SAMPLE_MINIMAL: &str = r#"
[global]
staging_dir = "/staging"
restic_repo = "sftp:demo@demo.repo.example.com:vk"
restic_password = "${RESTIC_PASSWORD}"
"#;

    fn lookup(k: &str) -> Option<String> {
        match k {
            "RESTIC_PASSWORD" => Some("s3cret".into()),
            _ => None,
        }
    }

    #[test]
    fn parses_and_interpolates() {
        let cfg = load_from_str(SAMPLE, &lookup).unwrap();
        assert_eq!(cfg.global.restic_password, "s3cret");
        assert_eq!(cfg.global.staging_dir.to_str().unwrap(), "/staging");
        assert_eq!(cfg.global.restic_timeout_minutes, Some(300));
        assert_eq!(
            cfg.notify.healthchecks_base.as_deref(),
            Some("https://hc-ping.com")
        );
        assert!(cfg.notify.ses.is_none());
    }

    #[test]
    fn missing_env_var_names_the_variable() {
        let err = load_from_str(SAMPLE, &|_| None).unwrap_err();
        assert!(err.to_string().contains("RESTIC_PASSWORD"));
    }

    #[test]
    fn restic_timeout_minutes_defaults_to_none_when_absent() {
        let cfg = load_from_str(SAMPLE_MINIMAL, &lookup).unwrap();
        assert_eq!(cfg.global.restic_timeout_minutes, None);
    }
}
