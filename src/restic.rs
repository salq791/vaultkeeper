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
    fn snapshots(&self, tag: Option<&str>) -> Result<Vec<Snapshot>>;
    #[allow(dead_code)]
    fn restore(&self, snapshot_id: &str, dest: &Path) -> Result<()>; // Consumed by Task 6: verify flow
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
        "--keep-daily".into(),
        r.daily.to_string(),
        "--keep-weekly".into(),
        r.weekly.to_string(),
        "--keep-monthly".into(),
        r.monthly.to_string(),
        "--json".into(),
    ]
}

#[allow(dead_code)]
pub fn latest_snapshot(repo: &dyn Repo, tag: &str) -> Result<Snapshot> {
    // Consumed by Task 6: verify flow
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
    timeout: std::time::Duration,
}

impl ResticCli {
    pub fn new(repo: String, password: String) -> Self {
        Self {
            repo,
            password,
            bin: "restic".into(),
            timeout: std::time::Duration::from_secs(240 * 60),
        }
    }

    pub fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn run(&self, args: &[String]) -> Result<String> {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args)
            .env("RESTIC_REPOSITORY", &self.repo)
            .env("RESTIC_PASSWORD", &self.password)
            .env_remove("VAULTKEEPER_MASTER_KEY");
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

impl Repo for ResticCli {
    fn ensure_init(&self) -> Result<()> {
        match self.run(&["cat".into(), "config".into()]) {
            Ok(_) => Ok(()),
            Err(probe_err) => match self.run(&["init".into()]) {
                Ok(_) => Ok(()),
                Err(init_err) => Err(init_err.context(format!(
                    "restic init failed after repo probe also failed: {probe_err:#}"
                ))),
            },
        }
    }

    fn backup(&self, path: &Path, tag: &str) -> Result<BackupSummary> {
        let out = self.run(&[
            "backup".into(),
            path.display().to_string(),
            "--tag".into(),
            tag.into(),
            "--json".into(),
        ])?;
        parse_backup_output(&out)
    }

    fn forget(&self, tag: &str, retention: &Retention) -> Result<()> {
        self.run(&forget_args(tag, retention)).map(|_| ())
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
