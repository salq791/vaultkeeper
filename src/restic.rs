use crate::types::Retention;
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Deserialize)]
pub struct BackupSummary {
    pub snapshot_id: String,
    pub total_bytes_processed: i64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Snapshot {
    pub id: String,
    pub time: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

pub trait Repo {
    fn ensure_init(&self) -> Result<()>;
    fn backup(&self, path: &Path, tag: &str) -> Result<BackupSummary>;
    fn forget(&self, tag: &str, retention: &Retention) -> Result<()>;
    fn prune(&self) -> Result<()>;
    fn check(&self) -> Result<()>;
    fn snapshots(&self, tag: Option<&str>) -> Result<Vec<Snapshot>>;
    fn restore(&self, snapshot_id: &str, dest: &Path) -> Result<()>;
}

pub fn parse_backup_output(out: &str) -> Result<BackupSummary> {
    for line in out.lines() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            if v.get("message_type").and_then(|m| m.as_str()) == Some("summary") {
                return serde_json::from_value(v).context("malformed restic summary");
            }
        }
    }
    bail!("restic backup produced no summary line");
}

pub fn parse_snapshots(out: &str) -> Result<Vec<Snapshot>> {
    serde_json::from_str(out).context("malformed restic snapshots output")
}

pub fn forget_args(tag: &str, r: &Retention) -> Vec<String> {
    vec![
        "forget".into(),
        "--tag".into(),
        tag.into(),
        // Source identity is encoded in the tag. Grouping only by tags keeps
        // retention stable across container hostnames and configured path
        // changes.
        "--group-by".into(),
        "tags".into(),
        "--keep-daily".into(),
        r.daily.to_string(),
        "--keep-weekly".into(),
        r.weekly.to_string(),
        "--keep-monthly".into(),
        r.monthly.to_string(),
        "--json".into(),
    ]
}

pub fn backup_args(path: &Path, tag: &str, host: &str) -> Vec<String> {
    vec![
        "backup".into(),
        path.display().to_string(),
        "--tag".into(),
        tag.into(),
        "--host".into(),
        host.into(),
        "--json".into(),
    ]
}

pub fn latest_snapshot(repo: &dyn Repo, tag: &str) -> Result<Snapshot> {
    let mut snaps = repo.snapshots(Some(tag))?;
    snaps.sort_by_key(|s| {
        chrono::DateTime::parse_from_rfc3339(&s.time)
            .map(|t| t.timestamp())
            .unwrap_or(i64::MIN)
    });
    snaps
        .pop()
        .with_context(|| format!("no snapshots found for {tag}"))
}

pub struct ResticCli {
    repo: String,
    password: String,
    bin: String,
    host: String,
    timeout: std::time::Duration,
}

impl ResticCli {
    pub fn new(repo: String, password: String, host: String) -> Self {
        Self {
            repo,
            password,
            bin: "restic".into(),
            host,
            timeout: std::time::Duration::from_secs(240 * 60),
        }
    }

    pub fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Initialize a repository only when explicitly requested by an
    /// operator. Routine backups never translate an authentication or
    /// network error into an attempted repository creation.
    pub fn initialize(&self) -> Result<()> {
        self.run(&["init".into()]).map(|_| ())
    }

    pub fn probe(&self) -> Result<()> {
        self.run(&["cat".into(), "config".into()]).map(|_| ())
    }

    fn run(&self, args: &[String]) -> Result<String> {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args)
            .env("RESTIC_REPOSITORY", &self.repo)
            .env("RESTIC_PASSWORD", &self.password);
        scrub_non_restic_secrets(&mut cmd);
        let out = crate::util::output_with_timeout(&mut cmd, self.timeout)?;
        if !out.status.success() {
            let truncated =
                crate::util::truncate_marked(&String::from_utf8_lossy(&out.stderr), 2000);
            bail!(
                "restic {} failed: {}",
                args.first().map(String::as_str).unwrap_or(""),
                truncated
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

fn scrub_non_restic_secrets(command: &mut Command) {
    command
        .env_remove("VAULTKEEPER_MASTER_KEY")
        .env_remove("VAULTKEEPER_RESTORE_TARGET");
}

impl Repo for ResticCli {
    fn ensure_init(&self) -> Result<()> {
        self.probe().context(
            "restic repository is unavailable or uninitialized; verify credentials/network, or run `vaultkeeper init-repository` once",
        )
    }

    fn backup(&self, path: &Path, tag: &str) -> Result<BackupSummary> {
        let out = self.run(&backup_args(path, tag, &self.host))?;
        parse_backup_output(&out)
    }

    fn forget(&self, tag: &str, retention: &Retention) -> Result<()> {
        self.run(&forget_args(tag, retention)).map(|_| ())
    }

    fn prune(&self) -> Result<()> {
        self.run(&["prune".into()]).map(|_| ())
    }

    fn check(&self) -> Result<()> {
        self.run(&["check".into()]).map(|_| ())
    }

    fn snapshots(&self, tag: Option<&str>) -> Result<Vec<Snapshot>> {
        let mut args = vec!["snapshots".into(), "--json".into()];
        if let Some(t) = tag {
            args.push("--tag".into());
            args.push(t.into());
        }
        parse_snapshots(&self.run(&args)?)
    }

    fn restore(&self, snapshot_id: &str, dest: &Path) -> Result<()> {
        self.run(&[
            "restore".into(),
            snapshot_id.into(),
            "--target".into(),
            dest.display().to_string(),
        ])
        .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Retention;

    #[test]
    fn parses_backup_summary_line() {
        let out = concat!(
            r#"{"message_type":"status","percent_done":1}"#,
            "\n",
            r#"{"message_type":"summary","snapshot_id":"a1b2c3","total_bytes_processed":52428800}"#,
            "\n"
        );
        let s = parse_backup_output(out).unwrap();
        assert_eq!(s.snapshot_id, "a1b2c3");
        assert_eq!(s.total_bytes_processed, 52428800);
    }

    #[test]
    fn missing_summary_is_error() {
        assert!(parse_backup_output(r#"{"message_type":"status"}"#).is_err());
    }

    #[test]
    fn parses_snapshot_list() {
        let out = r#"[{"id":"deadbeef","time":"2026-07-13T02:00:00Z","tags":["source=acme-db"]}]"#;
        let v = parse_snapshots(out).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].id, "deadbeef");
        assert_eq!(v[0].tags, vec!["source=acme-db"]);
    }

    #[test]
    fn forget_args_map_retention() {
        let r = Retention {
            daily: 7,
            weekly: 4,
            monthly: 6,
        };
        assert_eq!(
            forget_args("source=acme-db", &r),
            vec![
                "forget",
                "--tag",
                "source=acme-db",
                "--group-by",
                "tags",
                "--keep-daily",
                "7",
                "--keep-weekly",
                "4",
                "--keep-monthly",
                "6",
                "--json"
            ]
        );
    }

    #[test]
    fn backup_uses_stable_configured_host() {
        assert_eq!(
            backup_args(Path::new("/data/run-42"), "source=acme-db", "vaultkeeper"),
            vec![
                "backup",
                "/data/run-42",
                "--tag",
                "source=acme-db",
                "--host",
                "vaultkeeper",
                "--json"
            ]
        );
    }

    #[test]
    fn restic_child_does_not_inherit_vault_or_restore_credentials() {
        let mut command = Command::new("unused");
        scrub_non_restic_secrets(&mut command);
        for name in ["VAULTKEEPER_MASTER_KEY", "VAULTKEEPER_RESTORE_TARGET"] {
            assert!(
                command
                    .get_envs()
                    .any(|(key, value)| key == name && value.is_none()),
                "{name} must be explicitly removed from restic's environment"
            );
        }
    }

    struct FakeRepo(Vec<Snapshot>);
    impl Repo for FakeRepo {
        fn ensure_init(&self) -> Result<()> {
            Ok(())
        }
        fn backup(&self, _p: &std::path::Path, _t: &str) -> Result<BackupSummary> {
            unreachable!()
        }
        fn forget(&self, _t: &str, _r: &crate::types::Retention) -> Result<()> {
            unreachable!()
        }
        fn prune(&self) -> Result<()> {
            unreachable!()
        }
        fn check(&self) -> Result<()> {
            unreachable!()
        }
        fn snapshots(&self, _t: Option<&str>) -> Result<Vec<Snapshot>> {
            Ok(self.0.clone())
        }
        fn restore(&self, _id: &str, _d: &std::path::Path) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn latest_snapshot_picks_newest_by_parsed_time() {
        let repo = FakeRepo(vec![
            Snapshot {
                id: "old".into(),
                time: "2026-07-01T02:00:00+02:00".into(),
                tags: vec![],
            },
            Snapshot {
                id: "new".into(),
                time: "2026-07-13T22:00:00-04:00".into(),
                tags: vec![],
            },
            Snapshot {
                id: "mid".into(),
                time: "2026-07-10T02:00:00Z".into(),
                tags: vec![],
            },
        ]);
        assert_eq!(latest_snapshot(&repo, "source=x").unwrap().id, "new");
    }

    #[test]
    fn latest_snapshot_errors_when_empty() {
        let err = latest_snapshot(&FakeRepo(vec![]), "source=x").unwrap_err();
        assert!(err.to_string().contains("source=x"));
    }
}
