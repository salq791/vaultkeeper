# Vaultkeeper Plan 3: Scheduler Daemon + Notifications Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `vaultkeeper daemon` runs unattended, firing each enabled source on its cron schedule with per-child timeouts, and every run reports to healthchecks.io, an optional webhook, and SES email on failure.

**Architecture:** One tokio task per enabled source sleeps until its next cron occurrence, then executes the existing blocking pipeline inside `spawn_blocking` (MANDATORY: engines use blocking reqwest and std::process, which panic or stall on async workers). All engine and restic child processes go through a new `util::output_with_timeout` so a hung tool can never wedge the daemon. Notifications are a `Notifier` with pure, unit-tested URL/payload builders and thin send paths; notification failures are logged warnings, never run failures. The pipeline distinguishes `success_prune_failed` (backup snapshot exists, retention pruning failed) from real failure.

**Tech Stack:** Existing crate plus `tokio` (rt-multi-thread, macros, time, signal, sync), `croner` + `chrono` (cron parsing), `wait-timeout` (child timeouts), `aws-config` + `aws-sdk-sesv2` (email), reqwest gains the `json` feature.

**Spec:** `docs/superpowers/specs/2026-07-13-vaultkeeper-design.md`. Plan 3 of 5. Mandatory content from the plan-2 final review: spawn_blocking around run_backup, child-process timeouts, forget-failure journal rework, scheduler filters enabled sources.

## Global Constraints

- PUBLIC REPO: no secrets, tokens, real hostnames, or real project refs in ANY committed file. Test fixtures use example.com and synthetic values.
- Never use em dashes in any file, code comment, or doc. Use commas, colons, or hyphens.
- `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` must pass at every commit.
- Secrets never in argv, error messages, logs, or Debug surfaces. Notification payloads and emails carry status and detail tails only; detail already passes through truncate_marked and never contains secrets.
- Every notification send failure is `tracing::warn!`, never an error: a dead Slack webhook must not fail a backup.
- Tests must not require network access, real credentials, or external tools; test pure builders and use short-lived local child processes for timeout tests.
- Conventional commit messages with trailer: Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
- TDD with REAL captured RED output in every task report; fabricated evidence fails review.
- Default child timeout: 60 minutes per engine child (per-source settings key `timeout_minutes` overrides); restic child timeout: 240 minutes default (config `[global] restic_timeout_minutes` overrides).

---

### Task 1: util::output_with_timeout

**Files:**
- Modify: `Cargo.toml` (add `wait-timeout = "0.2"`), `src/util.rs`
- Test: inline `#[cfg(test)]` in `src/util.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `util::ChildOutput { status: std::process::ExitStatus, stdout: Vec<u8>, stderr: Vec<u8> }` and `util::output_with_timeout(cmd: &mut std::process::Command, timeout: std::time::Duration) -> anyhow::Result<ChildOutput>`. On timeout the child is killed and the error message names the program and the timeout seconds (never argv).

- [ ] **Step 1: Write the failing tests**

Append to the tests module in `src/util.rs`:

```rust
    fn shell(script: &str) -> std::process::Command {
        #[cfg(windows)]
        {
            let mut c = std::process::Command::new("cmd");
            c.arg("/C").arg(script);
            c
        }
        #[cfg(not(windows))]
        {
            let mut c = std::process::Command::new("sh");
            c.arg("-c").arg(script);
            c
        }
    }

    #[test]
    fn fast_child_completes_with_output() {
        let out = output_with_timeout(&mut shell("echo hi"), std::time::Duration::from_secs(30)).unwrap();
        assert!(out.status.success());
        assert!(String::from_utf8_lossy(&out.stdout).contains("hi"));
    }

    #[test]
    fn failing_child_reports_status_and_stderr() {
        let out = output_with_timeout(
            &mut shell("echo oops 1>&2 & exit 3"),
            std::time::Duration::from_secs(30),
        )
        .unwrap();
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("oops"));
    }

    #[test]
    fn hung_child_is_killed_and_errors() {
        #[cfg(windows)]
        let script = "ping -n 60 127.0.0.1 > NUL";
        #[cfg(not(windows))]
        let script = "sleep 60";
        let start = std::time::Instant::now();
        let err = output_with_timeout(&mut shell(script), std::time::Duration::from_secs(1)).unwrap_err();
        assert!(start.elapsed() < std::time::Duration::from_secs(20), "must not wait for the child");
        assert!(err.to_string().contains("timed out"));
    }
```

Note: on Windows `echo oops 1>&2 & exit 3` and on sh it is valid too (`&` runs sequentially enough for the test; if the exit-code assertion proves flaky on one platform, split the script per-cfg with `;` on sh). Keep the assertions identical.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test util`
Expected: compile error, `output_with_timeout`/`ChildOutput` not defined.

- [ ] **Step 3: Implement**

Add `wait-timeout = "0.2"` to `[dependencies]` and to `src/util.rs`:

```rust
pub struct ChildOutput {
    pub status: std::process::ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Run a child with piped output and a hard deadline. Reader threads drain
/// stdout/stderr so a chatty child cannot deadlock on a full pipe; on
/// timeout the child is killed and an error names the program and deadline.
pub fn output_with_timeout(
    cmd: &mut std::process::Command,
    timeout: std::time::Duration,
) -> anyhow::Result<ChildOutput> {
    use anyhow::Context;
    use std::io::Read;
    use wait_timeout::ChildExt;

    let program = cmd.get_program().to_string_lossy().into_owned();
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn {program}"))?;

    let mut out_pipe = child.stdout.take().expect("stdout piped");
    let mut err_pipe = child.stderr.take().expect("stderr piped");
    let out_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out_pipe.read_to_end(&mut buf);
        buf
    });
    let err_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = err_pipe.read_to_end(&mut buf);
        buf
    });

    let status = match child.wait_timeout(timeout).context("wait on child failed")? {
        Some(status) => status,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = out_thread.join();
            let _ = err_thread.join();
            anyhow::bail!("{program} timed out after {}s and was killed", timeout.as_secs());
        }
    };
    let stdout = out_thread.join().unwrap_or_default();
    let stderr = err_thread.join().unwrap_or_default();
    Ok(ChildOutput { status, stdout, stderr })
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test util` then the full gate.
Expected: 5 util tests green (2 prior + 3 new); the hung-child test finishes in about a second.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/util.rs
git commit -m "feat: child process execution with hard timeout"
```

---

### Task 2: Timeouts applied to every engine and restic child

**Files:**
- Modify: `src/engines/mod.rs`, `src/engines/postgres.rs`, `src/engines/mongodb.rs`, `src/engines/supabase_storage.rs`, `src/engines/supabase_functions.rs`, `src/restic.rs`, `src/config.rs`, `src/main.rs`
- Test: inline in `src/engines/mod.rs` and `src/config.rs`

**Interfaces:**
- Consumes: `util::output_with_timeout`, `util::ChildOutput`.
- Produces: `engines::timeout_from_settings(settings: &serde_json::Value) -> std::time::Duration` (key `timeout_minutes`, default 60); `ResticCli::with_timeout(self, d: Duration) -> Self` (default 240 minutes); `config::Global` gains `pub restic_timeout_minutes: Option<u64>`.

- [ ] **Step 1: Write the failing tests**

In `src/engines/mod.rs` tests:

```rust
    #[test]
    fn timeout_defaults_to_60_minutes() {
        assert_eq!(
            timeout_from_settings(&serde_json::json!({})),
            std::time::Duration::from_secs(3600)
        );
    }

    #[test]
    fn timeout_reads_settings_override() {
        assert_eq!(
            timeout_from_settings(&serde_json::json!({"timeout_minutes": 5})),
            std::time::Duration::from_secs(300)
        );
    }
```

In `src/config.rs` tests, extend `parses_and_interpolates` sample with `restic_timeout_minutes = 300` under `[global]` and assert `cfg.global.restic_timeout_minutes == Some(300)`; add an assertion in the existing minimal-config path (or a new test) that the field is `None` when absent.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test timeout`
Expected: compile error, `timeout_from_settings` not defined.

- [ ] **Step 3: Implement**

`src/engines/mod.rs`:

```rust
pub fn timeout_from_settings(settings: &serde_json::Value) -> std::time::Duration {
    let mins = settings
        .get("timeout_minutes")
        .and_then(|v| v.as_u64())
        .unwrap_or(60);
    std::time::Duration::from_secs(mins * 60)
}
```

`src/config.rs`: add `pub restic_timeout_minutes: Option<u64>,` to `Global`.

Each engine converts its `Command::new(...).args(...).output()` call to the two-step form and routes through the timeout helper. Pattern (postgres shown; apply the same mechanical change in mongodb, supabase_storage, and the CLI leg of supabase_functions, keeping every existing env/env_remove/current_dir exactly as is):

```rust
        let mut cmd = std::process::Command::new("pg_dump");
        cmd.args(&argv).envs(env).env_remove("VAULTKEEPER_MASTER_KEY");
        let out = crate::util::output_with_timeout(&mut cmd, super::timeout_from_settings(&ctx.settings))
            .context("failed to run pg_dump (is it installed and on PATH?)")?;
        if !out.status.success() {
            bail!(
                "pg_dump failed: {}",
                crate::util::truncate_marked(&String::from_utf8_lossy(&out.stderr), 2000)
            );
        }
```

(The spawn-failure `.context` strings keep their current per-tool wording; only the mechanism changes.)

`src/restic.rs`: add fields and builder:

```rust
pub struct ResticCli {
    repo: String,
    password: String,
    bin: String,
    timeout: std::time::Duration,
}

impl ResticCli {
    pub fn new(repo: String, password: String) -> Self {
        Self { repo, password, bin: "restic".into(), timeout: std::time::Duration::from_secs(240 * 60) }
    }

    pub fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = timeout;
        self
    }
    // run() switches to output_with_timeout(&mut cmd, self.timeout); env handling unchanged.
}
```

`src/main.rs`: wherever `ResticCli::new(...)` is constructed, apply the config override:

```rust
            let mut repo = restic::ResticCli::new(cfg.global.restic_repo, cfg.global.restic_password);
            if let Some(mins) = cfg.global.restic_timeout_minutes {
                repo = repo.with_timeout(std::time::Duration::from_secs(mins * 60));
            }
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: all green; existing invocation-builder tests untouched.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: hard timeouts for all engine and restic children"
```

---

### Task 3: Pipeline forget-failure rework (success_prune_failed)

**Files:**
- Modify: `src/pipeline.rs`
- Test: inline tests in `src/pipeline.rs`

**Interfaces:**
- Consumes: existing `Repo`, `Store`.
- Produces: `run_backup` returns `Ok(RunOutcome { status: "success_prune_failed", snapshot_id: Some(..), .. })` when backup succeeded but forget failed; journal row keeps the snapshot_id and carries the prune error in detail. Status strings later tasks rely on: `success`, `success_prune_failed`, `failed`.

- [ ] **Step 1: Write the failing test**

```rust
    struct PruneFailRepo;
    impl Repo for PruneFailRepo {
        fn ensure_init(&self) -> Result<()> {
            Ok(())
        }
        fn backup(&self, _path: &Path, _tag: &str) -> Result<BackupSummary> {
            Ok(BackupSummary { snapshot_id: "snap9".into(), total_bytes_processed: 7 })
        }
        fn forget(&self, _tag: &str, _r: &Retention) -> Result<()> {
            anyhow::bail!("repository is locked by another process")
        }
        fn snapshots(&self, _tag: Option<&str>) -> Result<Vec<Snapshot>> {
            Ok(vec![])
        }
    }

    #[test]
    fn prune_failure_after_successful_backup_is_partial_success() {
        let (st, src, staging) = setup();
        let out = run_backup(&st, &PruneFailRepo, &src, staging.path(), &OkEngine).unwrap();
        assert_eq!(out.status, "success_prune_failed");
        assert_eq!(out.snapshot_id.as_deref(), Some("snap9"));
        let runs = st.recent_runs(1).unwrap();
        assert_eq!(runs[0].status, "success_prune_failed");
        assert_eq!(runs[0].snapshot_id.as_deref(), Some("snap9"));
        assert!(runs[0].detail.as_deref().unwrap().contains("locked"));
        assert_eq!(runs[0].bytes, Some(7));
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test pipeline`
Expected: FAIL, current code journals `failed` and returns Err when forget fails.

- [ ] **Step 3: Implement**

In `run_backup`, change the closure to return the prune result separately instead of `?`-propagating it:

```rust
    let result = (|| -> Result<(String, i64, Option<String>)> {
        // ... staging, mirror, ctx, engine.dump, repo.backup exactly as today ...
        let summary = repo.backup(&backup_path, &tag)?;
        let prune_err = repo
            .forget(&tag, &source.retention)
            .err()
            .map(|e| crate::util::truncate_marked(&format!("{e:#}"), 4000));
        Ok((summary.snapshot_id, summary.total_bytes_processed, prune_err))
    })();
    let _ = std::fs::remove_dir_all(&staging_dir);

    match result {
        Ok((snapshot_id, bytes, None)) => {
            store.finish_run(run_id, "success", Some(bytes), Some(&snapshot_id), None)?;
            Ok(RunOutcome { run_id, snapshot_id: Some(snapshot_id), status: "success".into() })
        }
        Ok((snapshot_id, bytes, Some(prune_err))) => {
            store.finish_run(run_id, "success_prune_failed", Some(bytes), Some(&snapshot_id), Some(&prune_err))?;
            Ok(RunOutcome {
                run_id,
                snapshot_id: Some(snapshot_id),
                status: "success_prune_failed".into(),
            })
        }
        Err(e) => { /* unchanged failure arm from today, including the warn-on-journal-failure guard */ }
    }
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test pipeline` then the full gate. Existing success test still expects `forget:` in MockRepo calls and status `success`; both hold.

- [ ] **Step 5: Commit**

```bash
git add src/pipeline.rs
git commit -m "feat: distinguish prune failure from backup failure in journal"
```

---

### Task 4: Schedule validation, enable/disable, croner

**Files:**
- Create: `src/schedule.rs`
- Modify: `Cargo.toml` (add `croner = "2"`, `chrono = "0.4"`), `src/main.rs`, `src/store.rs`
- Test: inline in `src/schedule.rs` and `src/store.rs`; one new case in `tests/cli.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `schedule::validate(expr: &str) -> anyhow::Result<()>`; `schedule::next_occurrence(expr: &str, after: chrono::DateTime<chrono::Local>) -> anyhow::Result<chrono::DateTime<chrono::Local>>`; `Store::set_enabled(&self, name: &str, enabled: bool) -> anyhow::Result<()>`; CLI `source enable --name N` / `source disable --name N`; `source add` rejects invalid `--schedule`; `check-config` validates every stored schedule and verify_schedule.
- Pre-authorized adaptation (disclose in report): croner 2.x API surface; if `Cron::new(expr).parse()` / `find_next_occurrence(&after, false)` differ in the resolved version, adapt minimally while keeping `schedule.rs`'s two public functions exactly as declared.

- [ ] **Step 1: Write the failing tests**

`src/schedule.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn valid_five_field_cron_accepted() {
        assert!(validate("0 2 * * *").is_ok());
        assert!(validate("*/15 * * * *").is_ok());
    }

    #[test]
    fn garbage_rejected_naming_the_expression() {
        let err = validate("not a cron").unwrap_err();
        assert!(err.to_string().contains("not a cron"));
    }

    #[test]
    fn next_occurrence_advances_to_the_scheduled_time() {
        let after = chrono::Local.with_ymd_and_hms(2026, 1, 1, 0, 30, 0).unwrap();
        let next = next_occurrence("0 2 * * *", after).unwrap();
        assert_eq!(next, chrono::Local.with_ymd_and_hms(2026, 1, 1, 2, 0, 0).unwrap());
    }
}
```

`src/store.rs` tests:

```rust
    #[test]
    fn set_enabled_roundtrip() {
        let st = store();
        st.add_source(&sample()).unwrap();
        st.set_enabled("acme-db", false).unwrap();
        assert!(!st.get_source("acme-db").unwrap().enabled);
        st.set_enabled("acme-db", true).unwrap();
        assert!(st.get_source("acme-db").unwrap().enabled);
    }

    #[test]
    fn set_enabled_unknown_source_errors() {
        let st = store();
        assert!(st.set_enabled("ghost", false).is_err());
    }
```

`tests/cli.rs` new test `source_disable_then_list_shows_disabled`: add a source (stdin secrets), run `source disable --name acme-db`, assert `source list` stdout contains `disabled`; also assert `source add` with `--schedule "banana"` fails with stderr containing `banana`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test schedule`
Expected: compile error, module missing.

- [ ] **Step 3: Implement**

`src/schedule.rs`:

```rust
use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use croner::Cron;

pub fn validate(expr: &str) -> Result<()> {
    parse(expr).map(|_| ())
}

pub fn next_occurrence(expr: &str, after: DateTime<Local>) -> Result<DateTime<Local>> {
    parse(expr)?
        .find_next_occurrence(&after, false)
        .with_context(|| format!("no next occurrence for schedule '{expr}'"))
}

fn parse(expr: &str) -> Result<Cron> {
    Cron::new(expr)
        .parse()
        .with_context(|| format!("invalid cron schedule '{expr}'"))
}
```

`src/store.rs`:

```rust
    pub fn set_enabled(&self, name: &str, enabled: bool) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE sources SET enabled = ?2 WHERE name = ?1",
            params![name, enabled as i64],
        )?;
        anyhow::ensure!(n == 1, "no source named {name}");
        Ok(())
    }
```

`src/main.rs`: add `mod schedule;`; `SourceCmd::Enable { name } | SourceCmd::Disable { name }` variants calling `set_enabled` and printing the new state; in `SourceCmd::Add`, call `schedule::validate(&schedule)?` right after `engines::engine_for(&engine)?`; in `CheckConfig`, for every source validate `schedule` and any `verify_schedule`, printing `schedule ok`/`schedule INVALID: <err>` lines.

- [ ] **Step 4: Run to verify pass**

Run: full gate.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: cron validation, source enable/disable"
```

---

### Task 5: Notifier (healthchecks, webhook, SES)

**Files:**
- Create: `src/notify.rs`
- Modify: `Cargo.toml` (add `aws-config = "1"`, `aws-sdk-sesv2 = "1"`; reqwest features become `["blocking", "rustls-tls", "json"]`), `src/main.rs` (`mod notify;`)
- Test: inline in `src/notify.rs`

**Interfaces:**
- Consumes: `config::Notify`, `config::Ses`, `util::truncate_marked`.
- Produces:
  - `notify::RunEvent<'a> { Started, Finished { status: &'a str, snapshot_id: Option<&'a str>, detail: Option<&'a str> } }`
  - `notify::Notifier::from_notify(cfg: &config::Notify) -> anyhow::Result<Notifier>`
  - `Notifier::notify(&self, source_name: &str, hc_uuid: Option<&str>, event: &RunEvent)`: never returns an error; all send failures are `tracing::warn!`.
  - Pure, tested builders: `hc_url(base: &str, uuid: &str, event: &RunEvent) -> String` (Started -> `{base}/{uuid}/start`; Finished success and success_prune_failed -> `{base}/{uuid}`; Finished failed -> `{base}/{uuid}/fail`); `webhook_payload(source: &str, status: &str, snapshot_id: Option<&str>, detail: Option<&str>) -> serde_json::Value`; `email_subject_body(source: &str, status: &str, detail: Option<&str>) -> (String, String)`.
- Alerting policy (binding): webhook and SES fire ONLY for `failed` and `success_prune_failed`. Healthchecks gets a success ping for `success_prune_failed` because the dead-man switch measures backup freshness and a snapshot exists; the prune problem reaches the human via webhook/email instead. Record this rationale as a comment on `hc_url`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const B: &str = "https://hc-ping.com";

    #[test]
    fn hc_urls_per_event() {
        assert_eq!(hc_url(B, "u1", &RunEvent::Started), "https://hc-ping.com/u1/start");
        let ok = RunEvent::Finished { status: "success", snapshot_id: Some("s"), detail: None };
        assert_eq!(hc_url(B, "u1", &ok), "https://hc-ping.com/u1");
        let warn = RunEvent::Finished { status: "success_prune_failed", snapshot_id: Some("s"), detail: Some("d") };
        assert_eq!(hc_url(B, "u1", &warn), "https://hc-ping.com/u1");
        let bad = RunEvent::Finished { status: "failed", snapshot_id: None, detail: Some("boom") };
        assert_eq!(hc_url(B, "u1", &bad), "https://hc-ping.com/u1/fail");
    }

    #[test]
    fn hc_url_trims_trailing_slash() {
        assert_eq!(hc_url("https://hc-ping.com/", "u", &RunEvent::Started), "https://hc-ping.com/u/start");
    }

    #[test]
    fn webhook_payload_shape() {
        let p = webhook_payload("acme-db", "failed", None, Some("boom"));
        assert_eq!(p["source"], "acme-db");
        assert_eq!(p["status"], "failed");
        assert_eq!(p["snapshot_id"], serde_json::Value::Null);
        assert_eq!(p["detail"], "boom");
        assert_eq!(p["app"], "vaultkeeper");
    }

    #[test]
    fn email_subject_names_source_and_status_and_truncates() {
        let long = "x".repeat(3000);
        let (subject, body) = email_subject_body("acme-db", "failed", Some(&long));
        assert!(subject.contains("acme-db"));
        assert!(subject.contains("failed"));
        assert!(body.contains(" ...[truncated]"));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test notify`
Expected: compile error, module missing.

- [ ] **Step 3: Implement**

```rust
use crate::config::{Notify, Ses};
use anyhow::Result;

pub enum RunEvent<'a> {
    Started,
    Finished { status: &'a str, snapshot_id: Option<&'a str>, detail: Option<&'a str> },
}

/// success_prune_failed still pings healthchecks success: the dead-man switch
/// measures backup freshness and a snapshot exists. The prune problem reaches
/// the human via webhook/email, which DO fire for success_prune_failed.
pub fn hc_url(base: &str, uuid: &str, event: &RunEvent) -> String {
    let base = base.trim_end_matches('/');
    match event {
        RunEvent::Started => format!("{base}/{uuid}/start"),
        RunEvent::Finished { status: "failed", .. } => format!("{base}/{uuid}/fail"),
        RunEvent::Finished { .. } => format!("{base}/{uuid}"),
    }
}

pub fn webhook_payload(
    source: &str,
    status: &str,
    snapshot_id: Option<&str>,
    detail: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "app": "vaultkeeper",
        "source": source,
        "status": status,
        "snapshot_id": snapshot_id,
        "detail": detail,
    })
}

pub fn email_subject_body(source: &str, status: &str, detail: Option<&str>) -> (String, String) {
    let subject = format!("[vaultkeeper] {source}: {status}");
    let body = format!(
        "Backup source: {source}\nStatus: {status}\n\n{}",
        crate::util::truncate_marked(detail.unwrap_or("no detail"), 2000)
    );
    (subject, body)
}

pub struct Notifier {
    healthchecks_base: Option<String>,
    webhook_url: Option<String>,
    ses: Option<Ses>,
    client: reqwest::blocking::Client,
}

impl Notifier {
    pub fn from_notify(cfg: &Notify) -> Result<Notifier> {
        Ok(Notifier {
            healthchecks_base: cfg.healthchecks_base.clone(),
            webhook_url: cfg.webhook_url.clone(),
            ses: cfg.ses.clone(),
            client: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()?,
        })
    }

    pub fn notify(&self, source_name: &str, hc_uuid: Option<&str>, event: &RunEvent) {
        if let (Some(base), Some(uuid)) = (&self.healthchecks_base, hc_uuid) {
            let url = hc_url(base, uuid, event);
            let req = self.client.get(&url);
            let req = if let RunEvent::Finished { detail: Some(d), .. } = event {
                self.client.post(&url).body(crate::util::truncate_marked(d, 2000))
            } else {
                req
            };
            if let Err(e) = req.send() {
                tracing::warn!("healthchecks ping failed for {source_name}: {e}");
            }
        }
        if let RunEvent::Finished { status, snapshot_id, detail } = event {
            if *status == "failed" || *status == "success_prune_failed" {
                if let Some(url) = &self.webhook_url {
                    let payload = webhook_payload(source_name, status, *snapshot_id, *detail);
                    if let Err(e) = self.client.post(url).json(&payload).send() {
                        tracing::warn!("webhook post failed for {source_name}: {e}");
                    }
                }
                if let Some(ses) = &self.ses {
                    let (subject, body) = email_subject_body(source_name, status, *detail);
                    if let Err(e) = send_ses(ses, &subject, &body) {
                        tracing::warn!("ses email failed for {source_name}: {e}");
                    }
                }
            }
        }
    }
}

fn send_ses(ses: &Ses, subject: &str, body: &str) -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(async {
        let cfg = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new(ses.region.clone()))
            .load()
            .await;
        let client = aws_sdk_sesv2::Client::new(&cfg);
        let dest = aws_sdk_sesv2::types::Destination::builder()
            .set_to_addresses(Some(ses.to.clone()))
            .build();
        let content = aws_sdk_sesv2::types::EmailContent::builder()
            .simple(
                aws_sdk_sesv2::types::Message::builder()
                    .subject(aws_sdk_sesv2::types::Content::builder().data(subject).build()?)
                    .body(
                        aws_sdk_sesv2::types::Body::builder()
                            .text(aws_sdk_sesv2::types::Content::builder().data(body).build()?)
                            .build(),
                    )
                    .build(),
            )
            .build();
        client
            .send_email()
            .from_email_address(&ses.from)
            .destination(dest)
            .content(content)
            .send()
            .await?;
        Ok(())
    })
}
```

Requires `#[derive(Clone)]` on `config::Ses` (add it) and `tokio` with `rt` in dependencies (Task 6 adds the full feature set; this task may add `tokio = { version = "1", features = ["rt"] }` which Task 6 extends). Pre-authorized adaptation: aws-sdk-sesv2 builder API details may differ slightly in the resolved version; keep `send_ses(ses, subject, body)` private and adapt internals minimally, disclosing in the report.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test notify` then the full gate.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: notifier with healthchecks, webhook, and ses email"
```

---

### Task 6: Scheduler daemon

**Files:**
- Create: `src/scheduler.rs`, `src/exec.rs`
- Modify: `Cargo.toml` (tokio features become `["rt-multi-thread", "macros", "time", "signal", "sync"]`), `src/main.rs`
- Test: inline in `src/scheduler.rs` (pure helper) plus existing suites

**Interfaces:**
- Consumes: everything above.
- Produces:
  - `exec::execute_source(cfg: &config::Config, db_path: &str, source_name: &str) -> anyhow::Result<pipeline::RunOutcome>`: opens its own Store (one SQLite connection per run), builds engine + ResticCli (with restic timeout override) + Notifier, sends `RunEvent::Started`, runs `pipeline::run_backup`, sends `RunEvent::Finished` (mapping an Err to status "failed" with the error text as detail), returns the outcome. Used by BOTH the CLI `run` command and the daemon so manual and scheduled runs behave identically.
  - `scheduler::run_daemon(cfg: config::Config, db_path: String) -> anyhow::Result<()>` (async): validates every enabled source's schedule at startup, calls `repo.ensure_init()` once, then spawns one tokio task per ENABLED source; each task loops: compute `schedule::next_occurrence`, `tokio::time::sleep` until then (re-checking a shutdown watch channel via `tokio::select!`), then `tokio::task::spawn_blocking` around `exec::execute_source` and await it (MANDATORY: engines use blocking reqwest and std::process; running them on an async worker panics or stalls the runtime). Ctrl-C flips the watch channel; sleeping tasks exit immediately, in-flight blocking runs are awaited to completion, then the daemon returns.
  - `scheduler::sleep_duration(next: chrono::DateTime<chrono::Local>, now: chrono::DateTime<chrono::Local>) -> std::time::Duration` (pure, tested; clamps negative to zero).
  - CLI: `vaultkeeper daemon` subcommand. Sources are read once at startup; a startup `tracing::info!` states that source changes require a daemon restart.

- [ ] **Step 1: Write the failing test**

`src/scheduler.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn sleep_duration_positive_and_clamped() {
        let now = chrono::Local.with_ymd_and_hms(2026, 1, 1, 1, 0, 0).unwrap();
        let next = chrono::Local.with_ymd_and_hms(2026, 1, 1, 2, 0, 0).unwrap();
        assert_eq!(sleep_duration(next, now), std::time::Duration::from_secs(3600));
        assert_eq!(sleep_duration(now, next), std::time::Duration::ZERO);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test scheduler`
Expected: compile error, module missing.

- [ ] **Step 3: Implement**

`src/exec.rs`:

```rust
use crate::notify::{Notifier, RunEvent};
use crate::{config, crypto, engines, pipeline, restic, store};
use anyhow::Result;

pub fn execute_source(
    cfg: &config::Config,
    db_path: &str,
    source_name: &str,
) -> Result<pipeline::RunOutcome> {
    let st = store::Store::open(db_path, crypto::MasterKey::from_env()?)?;
    let source = st.get_source(source_name)?;
    let engine = engines::engine_for(&source.engine)?;
    let mut repo = restic::ResticCli::new(
        cfg.global.restic_repo.clone(),
        cfg.global.restic_password.clone(),
    );
    if let Some(mins) = cfg.global.restic_timeout_minutes {
        repo = repo.with_timeout(std::time::Duration::from_secs(mins * 60));
    }
    let notifier = Notifier::from_notify(&cfg.notify)?;
    notifier.notify(&source.name, source.healthchecks_uuid.as_deref(), &RunEvent::Started);

    // ensure_init runs inside the same result handling as run_backup so a
    // repo-init failure still reaches the Err arm below and fires a Finished
    // failed notification (webhook/email/hc-fail), not just the Started ping.
    use crate::restic::Repo as _;
    let result = (|| {
        repo.ensure_init()?;
        pipeline::run_backup(&st, &repo, &source, &cfg.global.staging_dir, engine.as_ref())
    })();
    match &result {
        Ok(outcome) => {
            // Scoped to this run's id, not the most recently written row:
            // recent_runs(1) races when sibling sources finish concurrently.
            let detail = st.run_detail(outcome.run_id).ok().flatten();
            notifier.notify(
                &source.name,
                source.healthchecks_uuid.as_deref(),
                &RunEvent::Finished {
                    status: &outcome.status,
                    snapshot_id: outcome.snapshot_id.as_deref(),
                    detail: detail.as_deref(),
                },
            );
        }
        Err(e) => {
            let detail = crate::util::truncate_marked(&format!("{e:#}"), 2000);
            notifier.notify(
                &source.name,
                source.healthchecks_uuid.as_deref(),
                &RunEvent::Finished { status: "failed", snapshot_id: None, detail: Some(&detail) },
            );
        }
    }
    result
}
```

`src/scheduler.rs`:

```rust
use crate::{config, crypto, exec, schedule, store};
use anyhow::{Context, Result};
use chrono::{DateTime, Local};

pub fn sleep_duration(next: DateTime<Local>, now: DateTime<Local>) -> std::time::Duration {
    (next - now).to_std().unwrap_or(std::time::Duration::ZERO)
}

pub async fn run_daemon(cfg: config::Config, db_path: String) -> Result<()> {
    let st = store::Store::open(&db_path, crypto::MasterKey::from_env()?)?;
    let sources: Vec<_> = st.list_sources()?.into_iter().filter(|s| s.enabled).collect();
    drop(st);
    anyhow::ensure!(!sources.is_empty(), "no enabled sources; add one with 'vaultkeeper source add'");
    for s in &sources {
        schedule::validate(&s.schedule).with_context(|| format!("source {}", s.name))?;
    }
    tracing::info!(
        "daemon starting with {} enabled source(s); source changes require a restart",
        sources.len()
    );

    // Fail fast if the repo is unreachable at daemon startup rather than
    // waiting for the first scheduled run to discover it hours later.
    let mut repo = crate::restic::ResticCli::new(
        cfg.global.restic_repo.clone(),
        cfg.global.restic_password.clone(),
    );
    if let Some(mins) = cfg.global.restic_timeout_minutes {
        repo = repo.with_timeout(std::time::Duration::from_secs(mins.saturating_mul(60)));
    }
    {
        use crate::restic::Repo as _;
        repo.ensure_init()
            .context("restic repository unreachable at daemon startup")?;
    }
    tracing::info!("restic repository reachable");

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let mut handles = Vec::new();
    let cfg = std::sync::Arc::new(cfg);
    for source in sources {
        let cfg = cfg.clone();
        let db_path = db_path.clone();
        let mut shutdown = shutdown_rx.clone();
        handles.push(tokio::spawn(async move {
            loop {
                let next = match schedule::next_occurrence(&source.schedule, Local::now()) {
                    Ok(n) => n,
                    Err(e) => {
                        tracing::error!("{}: schedule error, stopping this source: {e:#}", source.name);
                        return;
                    }
                };
                tracing::info!("{}: next run at {}", source.name, next);
                tokio::select! {
                    _ = tokio::time::sleep(sleep_duration(next, Local::now())) => {}
                    _ = shutdown.changed() => {
                        tracing::info!("{}: shutdown requested", source.name);
                        return;
                    }
                }
                let cfg2 = cfg.clone();
                let db2 = db_path.clone();
                let name = source.name.clone();
                let join = tokio::task::spawn_blocking(move || exec::execute_source(&cfg2, &db2, &name));
                match join.await {
                    Ok(Ok(outcome)) => tracing::info!("{}: run finished with status {}", source.name, outcome.status),
                    Ok(Err(e)) => tracing::error!("{}: run failed: {e:#}", source.name),
                    Err(e) => tracing::error!("{}: run panicked: {e}", source.name),
                }
            }
        }));
    }

    tokio::signal::ctrl_c().await.context("failed to listen for ctrl-c")?;
    tracing::info!("ctrl-c received: stopping schedules, waiting for in-flight runs");
    let _ = shutdown_tx.send(true);
    for h in handles {
        let _ = h.await;
    }
    Ok(())
}
```

`src/main.rs`: add `mod exec; mod scheduler; mod schedule;` (schedule added in Task 4); add `Daemon` variant:

```rust
        Command::Daemon => {
            let cfg = config::load(&config_path())?;
            let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
            rt.block_on(scheduler::run_daemon(cfg, db_path()))
        }
```

And rewrite `Command::Run` to delegate: `let out = exec::execute_source(&cfg, &db_path(), &source)?;` keeping its current println. Note `Config`, `Global`, `Notify`, `Ses` need `#[derive(Clone)]`? No: `run_daemon` takes ownership and wraps in Arc; only `Ses` needs Clone (Task 5 added it). Do not add Clone elsewhere.

- [ ] **Step 4: Run to verify pass**

Run: full gate. Then a manual smoke on this machine: `cargo run -- daemon` with no config must fail fast with the missing-config error (VAULTKEEPER_CONFIG unset defaults to /config/config.toml which does not exist on Windows); capture the error line in the report as evidence the wiring executes.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: cron scheduler daemon with graceful shutdown"
```

---

### Task 7: README, check-config polish

**Files:**
- Modify: `README.md`, `src/main.rs`

**Interfaces:**
- Consumes: everything above.
- Produces: README roadmap line `- [x] Built-in scheduler, healthchecks.io / webhook / SES alerting`; check-config prints which notification channels are configured (names only, never values), e.g. `notify: healthchecks configured, webhook configured, ses configured` or `notify: none configured`.

- [ ] **Step 1: Write the failing test**

Extend `tests/cli.rs` `check-config` coverage: add a test `check_config_reports_notify_channels` that writes a minimal config.toml (staging_dir, restic_repo, restic_password literal, `[notify] healthchecks_base = "https://hc-ping.com"`) to a temp dir, runs `check-config` with VAULTKEEPER_CONFIG pointing at it, asserts exit success and stdout contains `healthchecks configured`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --test cli check_config`
Expected: FAIL, no such output line yet.

- [ ] **Step 3: Implement**

In the `CheckConfig` arm after the tools loop:

```rust
            let mut channels = Vec::new();
            if cfg.notify.healthchecks_base.is_some() {
                channels.push("healthchecks configured");
            }
            if cfg.notify.webhook_url.is_some() {
                channels.push("webhook configured");
            }
            if cfg.notify.ses.is_some() {
                channels.push("ses configured");
            }
            if channels.is_empty() {
                println!("notify: none configured");
            } else {
                println!("notify: {}", channels.join(", "));
            }
```

README: check the scheduler/alerting roadmap line.

- [ ] **Step 4: Run to verify pass**

Run: full gate.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: check-config reports notify channels, roadmap update"
```

---

## Self-Review Notes

- Spec coverage: scheduler (daily/weekly via cron expressions, per spec), healthchecks dead-man switch per source with /start and /fail, optional webhook, SES email via aws-sdk-sesv2, per-source serialization (one task per source, runs awaited before rescheduling), enabled filtering (mandate), spawn_blocking (mandate), child timeouts everywhere (mandate), forget-failure journal rework (mandate). Verify-schedule execution is plan 4 (verify does not exist yet); check-config already validates stored verify_schedule strings.
- Type consistency: `RunEvent`/`Notifier` signatures match between Tasks 5 and 6; `execute_source` consumes `RunOutcome { status, snapshot_id, run_id }` from Task 3; `timeout_from_settings` and `with_timeout` used in Task 6 as declared in Task 2; `schedule::validate/next_occurrence` shared by Tasks 4 and 6.
- Placeholder scan: the Task 3 Err arm says "unchanged failure arm from today" alongside the exact guard it refers to (warn-on-journal-failure), which the implementer can see in the file being edited; all other steps carry complete code.
- Known risk consciously accepted: `exec::execute_source` reads the latest journal row for prune detail, which is correct because runs of a single source are serialized by the per-source task loop and by the CLI being a manual, human-invoked path.
