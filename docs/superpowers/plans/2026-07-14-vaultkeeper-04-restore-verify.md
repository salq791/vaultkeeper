# Vaultkeeper Plan 4: Restore + Scheduled Verify Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `vaultkeeper restore` brings any retained snapshot back into a target database (with same-host safety guards), and `vaultkeeper verify` restores the latest snapshot into scratch databases on a schedule, journaling row counts, so backups are continuously proven usable.

**Architecture:** The `Repo` trait gains `restore(snapshot_id, dest)`; the `Engine` trait gains `restore(&RestoreCtx)` and `verify(&VerifyCtx) -> Result<String>` (the String is a metrics line journaled as detail). `exec` gains `execute_verify` and `execute_restore` siblings of `execute_source`; the scheduler spawns a second task per source that has a `verify_schedule`. Mandated hardening from the plan-3 final review lands first: an atomic cross-process running guard in `start_run` (single conditional INSERT, 24h stale auto-clear), `ensure_init` moved inside `run_backup` so repo-level failures journal, a real journal-failure branch test, and fail-closed notification status handling.

**Tech Stack:** Existing crate plus `url = "2"` (parsing postgres target URLs so passwords never touch argv). No other new dependencies.

**Spec:** `docs/superpowers/specs/2026-07-13-vaultkeeper-design.md` (Restore and Verify section). Plan 4 of 5. Scratch databases come from a new optional `[verify]` config section (env-interpolated URLs); the docker-compose `verify` profile that provides them ships in plan 5.

## Global Constraints

- PUBLIC REPO: no secrets, tokens, real hostnames, or real project refs in ANY committed file. Fixtures use example.com and synthetic values.
- Never use em dashes in any file, code comment, or doc. Use commas, colons, or hyphens.
- `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` must pass at every commit.
- Secrets never in argv, error messages, logs, or Debug surfaces. Restore targets are URLs that CONTAIN passwords: they must be parsed and the password delivered via env (postgres) or 0600 config file (mongodb), never passed whole on argv.
- Restore refuses a target whose host equals the source's own host unless `--force-same-host`. Storage restore refuses to push to the remote unless `--confirm-remote-overwrite`.
- Status strings are exactly: `success`, `success_prune_failed`, `failed`, `verify_passed`, `verify_failed`, `stale` (guard-cleared zombie rows). Notification handling is FAIL-CLOSED: unknown statuses ping healthchecks /fail.
- Journal `kind` values: `backup`, `verify`, `restore`.
- Conventional commit messages with trailer: Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
- TDD with REAL captured RED output in every task report; fabricated evidence fails review.
- Tests must not require network access, real credentials, or external tools; test pure builders/parsers, use tempdir fixtures for directory checks. The CI e2e (Task 8) uses real restic and shim tools.

---

### Task 1: Cross-process running guard + repo-failure journaling + journal-failure test

**Files:**
- Modify: `src/store.rs`, `src/pipeline.rs`, `src/exec.rs`
- Test: inline in `src/store.rs` and `src/pipeline.rs`

**Interfaces:**
- Consumes: existing store/pipeline.
- Produces: `start_run` refuses a second concurrent run for the same source (any kind) with a clear error; `running` rows older than 24 hours are auto-marked `stale` and do not block; `run_backup` calls `repo.ensure_init()` as the FIRST step inside its journaled closure (so repo failures produce a journal row and exec's separate ensure_init call is removed); the failure-arm journal guard has a real test proving the original error survives a journal write failure.

- [ ] **Step 1: Write the failing tests**

`src/store.rs` tests:

```rust
    #[test]
    fn concurrent_run_for_same_source_refused() {
        let st = store();
        let sid = st.add_source(&sample()).unwrap();
        let _r1 = st.start_run(sid, "backup").unwrap();
        let err = st.start_run(sid, "verify").unwrap_err();
        assert!(err.to_string().contains("in progress"));
    }

    #[test]
    fn finished_run_unblocks_source() {
        let st = store();
        let sid = st.add_source(&sample()).unwrap();
        let r1 = st.start_run(sid, "backup").unwrap();
        st.finish_run(r1, "success", None, None, None).unwrap();
        assert!(st.start_run(sid, "backup").is_ok());
    }

    #[test]
    fn stale_running_row_is_cleared_and_does_not_block() {
        let st = store();
        let sid = st.add_source(&sample()).unwrap();
        st.conn_for_tests().execute(
            "INSERT INTO runs (source_id, kind, status, started_at) VALUES (?1, 'backup', 'running', datetime('now', '-25 hours'))",
            rusqlite::params![sid],
        ).unwrap();
        let r2 = st.start_run(sid, "backup").unwrap();
        assert!(r2 > 0);
        let stale: i64 = st.conn_for_tests().query_row(
            "SELECT count(*) FROM runs WHERE source_id = ?1 AND status = 'stale'",
            rusqlite::params![sid], |r| r.get(0),
        ).unwrap();
        assert_eq!(stale, 1);
    }
```

Add to `Store` (test seam, cfg-gated):

```rust
    #[cfg(test)]
    pub fn conn_for_tests(&self) -> &rusqlite::Connection {
        &self.conn
    }
```

`src/pipeline.rs` tests:

```rust
    struct InitFailRepo;
    impl Repo for InitFailRepo {
        fn ensure_init(&self) -> Result<()> {
            anyhow::bail!("repository unreachable")
        }
        fn backup(&self, _p: &Path, _t: &str) -> Result<BackupSummary> {
            unreachable!()
        }
        fn forget(&self, _t: &str, _r: &Retention) -> Result<()> {
            unreachable!()
        }
        fn snapshots(&self, _t: Option<&str>) -> Result<Vec<Snapshot>> {
            Ok(vec![])
        }
    }

    #[test]
    fn repo_init_failure_is_journaled() {
        let (st, src, staging) = setup();
        let err = run_backup(&st, &InitFailRepo, &src, staging.path(), &OkEngine).unwrap_err();
        assert!(err.to_string().contains("unreachable"));
        let runs = st.recent_runs(1).unwrap();
        assert_eq!(runs[0].status, "failed");
        assert!(runs[0].detail.as_deref().unwrap().contains("unreachable"));
    }

    struct SabotageEngine {
        db_path: std::path::PathBuf,
    }
    impl Engine for SabotageEngine {
        fn dump(&self, _ctx: &DumpCtx) -> Result<std::path::PathBuf> {
            let conn = rusqlite::Connection::open(&self.db_path).unwrap();
            conn.execute_batch("DROP TABLE runs;").unwrap();
            anyhow::bail!("connection refused")
        }
    }

    #[test]
    fn journal_write_failure_preserves_original_error() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("vk.db");
        let st = Store::open(db_path.to_str().unwrap(), MasterKey::from_hex(K).unwrap()).unwrap();
        st.add_source(&NewSource {
            name: "acme-db".into(),
            engine: "postgres".into(),
            schedule: "0 2 * * *".into(),
            verify_schedule: None,
            retention: Retention { daily: 7, weekly: 4, monthly: 6 },
            healthchecks_uuid: None,
            settings: serde_json::json!({}),
            secrets: std::collections::HashMap::new(),
        })
        .unwrap();
        let src = st.get_source("acme-db").unwrap();
        let staging = tempfile::tempdir().unwrap();
        let err = run_backup(&st, &MockRepo::default(), &src, staging.path(), &SabotageEngine { db_path })
            .unwrap_err();
        assert!(
            err.to_string().contains("connection refused"),
            "original error must survive a journal write failure, got: {err:#}"
        );
    }
```

(Note: `run_backup` starts the run row BEFORE dump, so the sabotage drop happens after `start_run` succeeded and before `finish_run` fails; the warn guard must swallow the journal error and return the dump error. Mock/test structs may need mirror-aware `dump` signatures matching the current trait.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test store && cargo test pipeline`
Expected: compile error (`conn_for_tests` missing) and, once compiling, guard tests FAIL because `start_run` currently allows concurrent runs; `repo_init_failure_is_journaled` FAILS because ensure_init is not called inside run_backup.

- [ ] **Step 3: Implement**

`src/store.rs`, replace `start_run`:

```rust
    /// Starts a run, refusing if the source already has a run in progress.
    /// A 'running' row older than 24 hours is treated as a crashed process's
    /// zombie: it is marked 'stale' and no longer blocks. The INSERT below is
    /// a single conditional statement, so the check-and-claim is atomic even
    /// across processes sharing the database file.
    pub fn start_run(&self, source_id: i64, kind: &str) -> Result<i64> {
        self.conn.execute(
            "UPDATE runs SET status = 'stale', finished_at = datetime('now')
             WHERE source_id = ?1 AND status = 'running'
             AND started_at <= datetime('now', '-24 hours')",
            params![source_id],
        )?;
        let inserted = self.conn.execute(
            "INSERT INTO runs (source_id, kind, status)
             SELECT ?1, ?2, 'running'
             WHERE NOT EXISTS (
               SELECT 1 FROM runs WHERE source_id = ?1 AND status = 'running'
             )",
            params![source_id, kind],
        )?;
        anyhow::ensure!(
            inserted == 1,
            "another run for this source is in progress; a run that crashed more than 24 hours ago clears automatically"
        );
        Ok(self.conn.last_insert_rowid())
    }
```

`src/pipeline.rs`: inside the journaled closure, make `repo.ensure_init()?;` the first statement (before staging setup). `src/exec.rs`: delete its now-duplicate `repo.ensure_init()?;` block (the `use crate::restic::Repo as _;` import moves into pipeline.rs if not already there; keep exec compiling).

- [ ] **Step 4: Run to verify pass**

Run: full gate. The plan-3 daemon boot fail-fast in scheduler.rs still calls ensure_init at startup; that stays.

- [ ] **Step 5: Commit**

```bash
git add src/store.rs src/pipeline.rs src/exec.rs
git commit -m "feat: atomic per-source running guard, journal repo failures"
```

---

### Task 2: Fail-closed notification statuses

**Files:**
- Modify: `src/notify.rs`
- Test: inline in `src/notify.rs`

**Interfaces:**
- Consumes: existing Notifier.
- Produces: `notify::is_success_ping(status: &str) -> bool` (true ONLY for `success`, `success_prune_failed`, `verify_passed`); `notify::alerts_humans(status: &str) -> bool` (true for `failed`, `success_prune_failed`, `verify_failed`, `verify_passed`); `hc_url` uses `is_success_ping`, so unknown statuses go to `/fail` (fail-closed); webhook/SES gating uses `alerts_humans`. `verify_passed` alerting is the spec's "verify report" email.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn unknown_status_pings_fail_closed() {
        let ev = RunEvent::Finished { status: "exploded_weirdly", snapshot_id: None, detail: None };
        assert_eq!(hc_url(B, "u1", &ev), "https://hc-ping.com/u1/fail");
    }

    #[test]
    fn verify_statuses_route_correctly() {
        let pass = RunEvent::Finished { status: "verify_passed", snapshot_id: None, detail: Some("tables=3") };
        assert_eq!(hc_url(B, "u1", &pass), "https://hc-ping.com/u1");
        let fail = RunEvent::Finished { status: "verify_failed", snapshot_id: None, detail: Some("no tables") };
        assert_eq!(hc_url(B, "u1", &fail), "https://hc-ping.com/u1/fail");
    }

    #[test]
    fn alert_gating_per_status() {
        assert!(alerts_humans("failed"));
        assert!(alerts_humans("success_prune_failed"));
        assert!(alerts_humans("verify_failed"));
        assert!(alerts_humans("verify_passed"));
        assert!(!alerts_humans("success"));
        assert!(!alerts_humans("stale"));
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test notify`
Expected: compile error (`alerts_humans` missing); the unknown-status test would fail on the current catch-all.

- [ ] **Step 3: Implement**

```rust
/// Statuses that count as a healthy check-in for the dead-man switch.
/// Everything else, including statuses this version does not know,
/// pings /fail: fail closed.
pub fn is_success_ping(status: &str) -> bool {
    matches!(status, "success" | "success_prune_failed" | "verify_passed")
}

/// Statuses that reach humans via webhook and email. verify_passed is
/// included deliberately: it is the spec's scheduled verify report.
pub fn alerts_humans(status: &str) -> bool {
    matches!(status, "failed" | "success_prune_failed" | "verify_failed" | "verify_passed")
}
```

`hc_url`'s match arms become: `Started` -> `/start`; `Finished { status, .. } if is_success_ping(status)` -> plain; `Finished { .. }` -> `/fail`. The webhook/SES `if` in `Notifier::notify` becomes `if alerts_humans(status)`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test notify` then the full gate.

- [ ] **Step 5: Commit**

```bash
git add src/notify.rs
git commit -m "feat: fail-closed notification status handling"
```

---

### Task 3: Repo::restore and latest_snapshot

**Files:**
- Modify: `src/restic.rs`, `src/pipeline.rs` (mock impls)
- Test: inline in `src/restic.rs`

**Interfaces:**
- Consumes: existing Repo trait.
- Produces: trait method `fn restore(&self, snapshot_id: &str, dest: &Path) -> anyhow::Result<()>` (ResticCli runs `restic restore <id> --target <dest>`); `restic::latest_snapshot(repo: &dyn Repo, tag: &str) -> anyhow::Result<Snapshot>` picking the max by RFC3339 `time` (parsed with chrono, not lexical compare), erroring "no snapshots for <tag>" when empty. All existing mock Repos in pipeline.rs gain a trivial `restore` impl returning Ok(()).

- [ ] **Step 1: Write the failing tests**

```rust
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
            Snapshot { id: "old".into(), time: "2026-07-01T02:00:00+02:00".into(), tags: vec![] },
            Snapshot { id: "new".into(), time: "2026-07-13T22:00:00-04:00".into(), tags: vec![] },
            Snapshot { id: "mid".into(), time: "2026-07-10T02:00:00Z".into(), tags: vec![] },
        ]);
        assert_eq!(latest_snapshot(&repo, "source=x").unwrap().id, "new");
    }

    #[test]
    fn latest_snapshot_errors_when_empty() {
        let err = latest_snapshot(&FakeRepo(vec![]), "source=x").unwrap_err();
        assert!(err.to_string().contains("source=x"));
    }
```

`Snapshot` needs `#[derive(Clone)]` for the fake; add it.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test restic`
Expected: compile errors (`restore` not on trait, `latest_snapshot` missing).

- [ ] **Step 3: Implement**

Trait addition and ResticCli impl:

```rust
    fn restore(&self, snapshot_id: &str, dest: &Path) -> Result<()> {
        self.run(&[
            "restore".into(),
            snapshot_id.into(),
            "--target".into(),
            dest.display().to_string(),
        ])
        .map(|_| ())
    }
```

```rust
pub fn latest_snapshot(repo: &dyn Repo, tag: &str) -> Result<Snapshot> {
    let mut snaps = repo.snapshots(Some(tag))?;
    snaps.sort_by_key(|s| {
        chrono::DateTime::parse_from_rfc3339(&s.time)
            .map(|t| t.timestamp())
            .unwrap_or(i64::MIN)
    });
    snaps.pop().with_context(|| format!("no snapshots found for {tag}"))
}
```

Add `fn restore(&self, _id: &str, _d: &Path) -> Result<()> { Ok(()) }` to MockRepo, PruneFailRepo, InitFailRepo in pipeline.rs.

- [ ] **Step 4: Run to verify pass**

Run: full gate.

- [ ] **Step 5: Commit**

```bash
git add src/restic.rs src/pipeline.rs
git commit -m "feat: repo restore and latest snapshot selection"
```

---

### Task 4: Engine::restore (postgres, mongodb, storage, functions)

**Files:**
- Modify: `Cargo.toml` (add `url = "2"`), `src/engines/mod.rs`, `src/engines/postgres.rs`, `src/engines/mongodb.rs`, `src/engines/supabase_storage.rs`, `src/engines/supabase_functions.rs`, `src/util.rs`
- Test: inline in each engine file and util.rs

**Interfaces:**
- Consumes: `util::write_new_0600`, `util::output_with_timeout`, `timeout_from_settings`.
- Produces:
  - `engines::RestoreCtx { restored_dir: PathBuf, source_name: String, target: Option<String>, force_same_host: bool, confirm_remote_overwrite: bool, settings: serde_json::Value, secrets: HashMap<String, String> }`
  - trait method `fn restore(&self, ctx: &RestoreCtx) -> anyhow::Result<()>`
  - `util::find_named(root: &Path, name: &str) -> anyhow::Result<PathBuf>`: recursive walk for the first entry (file or dir) with that name; errors naming both when absent. Restic restores recreate the original absolute path under dest, so engines locate their payload with `find_named(restored_dir, source_name)` (the staging/mirror dir was named after the source).
  - `postgres::pg_restore_invocation(target_url: &str, dump_file: &Path) -> anyhow::Result<(Vec<String>, Vec<(String, String)>)>`: parses the URL with the `url` crate; argv is `["--clean","--if-exists","-h",host,"-p",port,"-U",user,"-d",dbname,dump_file]`; password goes ONLY into env `PGPASSWORD`; errors if host/user/password/dbname missing.
  - `postgres::url_host(url: &str) -> Option<String>` and `mongodb::uri_host(uri: &str) -> Option<String>` (naive: text between '@' and the next ':' or '/' or end) for same-host comparison.
  - Behavior: postgres/mongodb require `target` and bail "target host matches the source host; pass --force-same-host to override" when hosts match and the flag is false. Storage ignores `target`, requires `confirm_remote_overwrite`, and rclone-syncs the restored mirror BACK to the source endpoint (same env config as dump, reversed direction). Functions restore prints manual redeploy steps (supabase functions deploy per function, auth-config.json as reference) and returns Ok.

- [ ] **Step 1: Write the failing tests**

`src/util.rs`:

```rust
    #[test]
    fn find_named_locates_nested_entry() {
        let d = tempfile::tempdir().unwrap();
        let deep = d.path().join("a").join("b").join("target-dir");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(find_named(d.path(), "target-dir").unwrap(), deep);
        assert!(find_named(d.path(), "missing").is_err());
    }
```

`src/engines/postgres.rs`:

```rust
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
                "--clean", "--if-exists", "-h", "restore.example.com", "-p", "5433",
                "-U", "admin", "-d", "newdb", "/r/acme-db/db.dump"
            ]
        );
        assert!(env.contains(&("PGPASSWORD".to_string(), "s3cret".to_string())));
        assert!(!argv.iter().any(|a| a.contains("s3cret")));
    }

    #[test]
    fn url_host_extracts() {
        assert_eq!(url_host("postgres://u:p@db.example.com:5432/x").as_deref(), Some("db.example.com"));
    }
```

(Windows path note: build the dump-file argv expectation with the same `Path::new(...).display().to_string()` if literal comparison fails.)

`src/engines/mongodb.rs`:

```rust
    #[test]
    fn uri_host_extracts() {
        assert_eq!(uri_host("mongodb://u:p@mongo.example.com:27017/app").as_deref(), Some("mongo.example.com"));
        assert_eq!(uri_host("mongodb+srv://u:p@cluster.example.com/app").as_deref(), Some("cluster.example.com"));
    }
```

Same-host guard tests, one per engine file (postgres shown, mongodb mirrors it with uris):

```rust
    #[test]
    fn restore_refuses_same_host_without_force() {
        let ctx = RestoreCtx {
            restored_dir: std::path::PathBuf::from("/nonexistent"),
            source_name: "acme-db".into(),
            target: Some("postgres://u:p@db.example.com:5432/other".into()),
            force_same_host: false,
            confirm_remote_overwrite: false,
            settings: serde_json::json!({"host": "db.example.com", "port": 5432, "dbname": "app", "user": "u"}),
            secrets: std::collections::HashMap::new(),
        };
        let err = PostgresEngine.restore(&ctx).unwrap_err();
        assert!(err.to_string().contains("force-same-host"));
    }
```

`src/engines/supabase_storage.rs`:

```rust
    #[test]
    fn storage_restore_requires_confirmation() {
        let ctx = RestoreCtx {
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
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test engines && cargo test util`
Expected: compile errors (RestoreCtx, restore, helpers missing).

- [ ] **Step 3: Implement**

`src/util.rs`:

```rust
pub fn find_named(root: &std::path::Path, name: &str) -> anyhow::Result<std::path::PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.file_name().to_string_lossy() == name {
                return Ok(entry.path());
            }
            if entry.file_type()?.is_dir() {
                stack.push(entry.path());
            }
        }
    }
    anyhow::bail!("could not find '{name}' under {}", root.display())
}
```

`src/engines/mod.rs`:

```rust
pub struct RestoreCtx {
    pub restored_dir: PathBuf,
    pub source_name: String,
    pub target: Option<String>,
    pub force_same_host: bool,
    pub confirm_remote_overwrite: bool,
    pub settings: serde_json::Value,
    pub secrets: HashMap<String, String>,
}

pub trait Engine {
    fn dump(&self, ctx: &DumpCtx) -> Result<PathBuf>;
    fn restore(&self, ctx: &RestoreCtx) -> Result<()>;
}
```

`src/engines/postgres.rs`:

```rust
pub fn url_host(url: &str) -> Option<String> {
    url::Url::parse(url).ok()?.host_str().map(|h| h.to_string())
}

pub fn pg_restore_invocation(
    target_url: &str,
    dump_file: &Path,
) -> Result<(Vec<String>, Vec<(String, String)>)> {
    let u = url::Url::parse(target_url).context("invalid target url")?;
    let host = u.host_str().context("target url missing host")?.to_string();
    let port = u.port().unwrap_or(5432).to_string();
    let user = (!u.username().is_empty())
        .then(|| u.username().to_string())
        .context("target url missing user")?;
    let password = u.password().context("target url missing password")?.to_string();
    let dbname = u.path().trim_start_matches('/').to_string();
    anyhow::ensure!(!dbname.is_empty(), "target url missing database name");
    let argv = vec![
        "--clean".into(), "--if-exists".into(),
        "-h".into(), host, "-p".into(), port,
        "-U".into(), user, "-d".into(), dbname,
        dump_file.display().to_string(),
    ];
    Ok((argv, vec![("PGPASSWORD".to_string(), password)]))
}

impl Engine for PostgresEngine {
    // dump unchanged
    fn restore(&self, ctx: &RestoreCtx) -> Result<()> {
        let target = ctx.target.as_deref().context("postgres restore requires --target <postgres-url>")?;
        let source_host = ctx.settings.get("host").and_then(|v| v.as_str()).unwrap_or("");
        if !ctx.force_same_host && url_host(target).as_deref() == Some(source_host) {
            anyhow::bail!("target host matches the source host; pass --force-same-host to override");
        }
        let payload = crate::util::find_named(&ctx.restored_dir, &ctx.source_name)?;
        let dump_file = payload.join("db.dump");
        let (argv, env) = pg_restore_invocation(target, &dump_file)?;
        let mut cmd = std::process::Command::new("pg_restore");
        cmd.args(&argv).envs(env).env_remove("VAULTKEEPER_MASTER_KEY").env_remove("RESTIC_PASSWORD");
        let out = crate::util::output_with_timeout(&mut cmd, super::timeout_from_settings(&ctx.settings))?;
        if !out.status.success() {
            anyhow::bail!(
                "pg_restore failed: {}",
                crate::util::truncate_marked(&String::from_utf8_lossy(&out.stderr), 2000)
            );
        }
        Ok(())
    }
}
```

`src/engines/mongodb.rs` (uri never on argv; config file via write_new_0600 into the payload dir, removed fail-closed like dump):

```rust
pub fn uri_host(uri: &str) -> Option<String> {
    let after_at = uri.split('@').nth(1)?;
    let end = after_at.find([':', '/']).unwrap_or(after_at.len());
    Some(after_at[..end].to_string())
}

impl Engine for MongodbEngine {
    // dump unchanged
    fn restore(&self, ctx: &RestoreCtx) -> Result<()> {
        let target = ctx.target.as_deref().context("mongodb restore requires --target <mongodb-uri>")?;
        let source_host = ctx.secrets.get("uri").and_then(|u| uri_host(u)).unwrap_or_default();
        if !ctx.force_same_host && uri_host(target).as_deref() == Some(source_host.as_str()) && !source_host.is_empty() {
            anyhow::bail!("target host matches the source host; pass --force-same-host to override");
        }
        let payload = crate::util::find_named(&ctx.restored_dir, &ctx.source_name)?;
        let dump_dir = payload.join("dump");
        let config_path = payload.join(".mongorestore-config.yml");
        crate::util::write_new_0600(&config_path, format!("uri: {target}\n").as_bytes())?;
        let mut cmd = std::process::Command::new("mongorestore");
        cmd.args([
            "--config".to_string(), config_path.display().to_string(),
            "--drop".to_string(), "--dir".to_string(), dump_dir.display().to_string(),
        ])
        .env_remove("VAULTKEEPER_MASTER_KEY")
        .env_remove("RESTIC_PASSWORD");
        let out = crate::util::output_with_timeout(&mut cmd, super::timeout_from_settings(&ctx.settings));
        let _ = std::fs::remove_file(&config_path);
        if config_path.exists() {
            anyhow::bail!("mongorestore config file could not be removed; aborting");
        }
        let out = out?;
        if !out.status.success() {
            anyhow::bail!(
                "mongorestore failed: {}",
                crate::util::truncate_marked(&String::from_utf8_lossy(&out.stderr), 2000)
            );
        }
        Ok(())
    }
}
```

`src/engines/supabase_storage.rs` (reverse sync, source creds, explicit confirmation):

```rust
    fn restore(&self, ctx: &RestoreCtx) -> Result<()> {
        anyhow::ensure!(
            ctx.confirm_remote_overwrite,
            "storage restore OVERWRITES the remote bucket contents; pass --confirm-remote-overwrite to proceed"
        );
        let mirror = crate::util::find_named(&ctx.restored_dir, &ctx.source_name)?;
        let (_, env) = rclone_invocation(&ctx.settings, &ctx.secrets, &mirror)?;
        let mut cmd = std::process::Command::new("rclone");
        cmd.args(["sync".to_string(), mirror.display().to_string(), "SUPA:".to_string()])
            .envs(env)
            .env_remove("VAULTKEEPER_MASTER_KEY")
            .env_remove("RESTIC_PASSWORD");
        let out = crate::util::output_with_timeout(&mut cmd, super::timeout_from_settings(&ctx.settings))?;
        if !out.status.success() {
            anyhow::bail!(
                "rclone restore sync failed: {}",
                crate::util::truncate_marked(&String::from_utf8_lossy(&out.stderr), 2000)
            );
        }
        Ok(())
    }
```

`src/engines/supabase_functions.rs`:

```rust
    fn restore(&self, ctx: &RestoreCtx) -> Result<()> {
        let payload = crate::util::find_named(&ctx.restored_dir, &ctx.source_name)?;
        println!("Edge Functions are redeployed with the supabase CLI, not written back by vaultkeeper.");
        println!("Restored source is at: {}", payload.display());
        println!("Steps:");
        println!("  1. cd into the restored directory shown above");
        println!("  2. supabase functions deploy --project-ref <your-project-ref> (per function or all)");
        println!("  3. auth-config.json in the same directory is a reference for manual settings re-entry");
        Ok(())
    }
```

- [ ] **Step 4: Run to verify pass**

Run: full gate.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: engine restore with same-host and overwrite guards"
```

---

### Task 5: Engine::verify + [verify] config

**Files:**
- Modify: `src/config.rs`, `src/engines/mod.rs`, all four engine files
- Test: inline in each engine file and config.rs

**Interfaces:**
- Consumes: Task 4 helpers (`find_named`, `pg_restore_invocation`).
- Produces:
  - `config::VerifyCfg { pub postgres_url: Option<String>, pub mongodb_uri: Option<String> }`, `Config.verify: VerifyCfg` (serde default, env-interpolated like everything else).
  - `engines::VerifyCtx { restored_dir: PathBuf, source_name: String, scratch_postgres: Option<String>, scratch_mongodb: Option<String>, settings: serde_json::Value, secrets: HashMap<String, String> }`
  - trait method `fn verify(&self, ctx: &VerifyCtx) -> anyhow::Result<String>` returning a metrics line (journaled as detail) on pass, Err on fail.
  - `mongodb::parse_restored_docs(out: &str) -> Option<u64>` (parses mongorestore's "N document(s) restored successfully" line).
  - `util::dir_stats(root: &Path) -> anyhow::Result<(u64, u64)>` (file count, total bytes, recursive).
  - Behavior: postgres verify needs `scratch_postgres` (else Err "configure [verify] postgres_url"); restores into scratch via `pg_restore_invocation`, then runs `psql -Atc` twice (ANALYZE; then `SELECT count(*) FROM information_schema.tables WHERE table_schema = 'public'` and `SELECT coalesce(sum(n_live_tup),0) FROM pg_stat_user_tables`), asserts table count > 0, returns `"tables=<t> approx_rows=<r>"`. mongodb verify needs `scratch_mongodb`; mongorestore --drop into scratch, parses restored-doc count from output, asserts > 0, returns `"docs=<n>"`. storage verify: `dir_stats` on the restored mirror, asserts files > 0, returns `"files=<n> bytes=<b>"`. functions verify: asserts `supabase/functions` exists with at least one entry and `auth-config.json` exists, returns `"functions=<n> auth_config=present"`.

- [ ] **Step 1: Write the failing tests**

`src/config.rs`: extend the sample with `[verify]\npostgres_url = "postgres://v:v@scratch.example.com:5432/scratch"` and assert `cfg.verify.postgres_url.as_deref() == Some(...)`; assert `verify` defaults when absent.

`src/engines/mongodb.rs`:

```rust
    #[test]
    fn parses_mongorestore_doc_count() {
        let out = "2026-07-14T02:00:01.000+0000\t55 document(s) restored successfully. 0 document(s) failed to restore.";
        assert_eq!(parse_restored_docs(out), Some(55));
        assert_eq!(parse_restored_docs("no numbers here"), None);
    }
```

`src/util.rs`:

```rust
    #[test]
    fn dir_stats_counts_files_and_bytes() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("sub")).unwrap();
        std::fs::write(d.path().join("a.bin"), b"12345").unwrap();
        std::fs::write(d.path().join("sub").join("b.bin"), b"123").unwrap();
        assert_eq!(dir_stats(d.path()).unwrap(), (2, 8));
    }
```

`src/engines/supabase_functions.rs`:

```rust
    #[test]
    fn verify_checks_functions_and_auth_config() {
        let d = tempfile::tempdir().unwrap();
        let payload = d.path().join("acme-fns");
        std::fs::create_dir_all(payload.join("supabase").join("functions").join("hello")).unwrap();
        std::fs::write(payload.join("auth-config.json"), b"{}").unwrap();
        let ctx = VerifyCtx {
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
```

`src/engines/supabase_storage.rs`:

```rust
    #[test]
    fn verify_reports_file_stats() {
        let d = tempfile::tempdir().unwrap();
        let mirror = d.path().join("acme-storage");
        std::fs::create_dir_all(&mirror).unwrap();
        std::fs::write(mirror.join("obj1"), b"abcd").unwrap();
        let ctx = VerifyCtx {
            restored_dir: d.path().to_path_buf(),
            source_name: "acme-storage".into(),
            scratch_postgres: None,
            scratch_mongodb: None,
            settings: serde_json::json!({}),
            secrets: std::collections::HashMap::new(),
        };
        assert_eq!(SupabaseStorageEngine.verify(&ctx).unwrap(), "files=1 bytes=4");
    }
```

Plus per-engine missing-scratch tests: postgres/mongodb verify with `scratch_*: None` errors containing `[verify]`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test`
Expected: compile errors (VerifyCtx, verify, helpers missing).

- [ ] **Step 3: Implement**

`src/config.rs`:

```rust
#[derive(Debug, Default, Deserialize)]
pub struct VerifyCfg {
    pub postgres_url: Option<String>,
    pub mongodb_uri: Option<String>,
}
```

with `#[serde(default)] pub verify: VerifyCfg` on `Config`.

`src/util.rs`:

```rust
pub fn dir_stats(root: &std::path::Path) -> anyhow::Result<(u64, u64)> {
    let mut files = 0u64;
    let mut bytes = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let ft = entry.file_type()?;
            if ft.is_dir() {
                stack.push(entry.path());
            } else if ft.is_file() {
                files += 1;
                bytes += entry.metadata()?.len();
            }
        }
    }
    Ok((files, bytes))
}
```

`src/engines/mod.rs`: `VerifyCtx` as declared; trait gains `fn verify(&self, ctx: &VerifyCtx) -> Result<String>;`.

`src/engines/postgres.rs` verify:

```rust
    fn verify(&self, ctx: &VerifyCtx) -> Result<String> {
        let scratch = ctx
            .scratch_postgres
            .as_deref()
            .context("postgres verify needs a scratch database: configure [verify] postgres_url")?;
        let payload = crate::util::find_named(&ctx.restored_dir, &ctx.source_name)?;
        let (argv, env) = pg_restore_invocation(scratch, &payload.join("db.dump"))?;
        let mut cmd = std::process::Command::new("pg_restore");
        cmd.args(&argv).envs(env.clone()).env_remove("VAULTKEEPER_MASTER_KEY").env_remove("RESTIC_PASSWORD");
        let out = crate::util::output_with_timeout(&mut cmd, super::timeout_from_settings(&ctx.settings))?;
        if !out.status.success() {
            anyhow::bail!(
                "verify pg_restore failed: {}",
                crate::util::truncate_marked(&String::from_utf8_lossy(&out.stderr), 2000)
            );
        }
        let psql = |sql: &str| -> Result<String> {
            let u = url::Url::parse(scratch)?;
            let mut cmd = std::process::Command::new("psql");
            cmd.args([
                "-Atc".to_string(), sql.to_string(),
                "-h".to_string(), u.host_str().unwrap_or_default().to_string(),
                "-p".to_string(), u.port().unwrap_or(5432).to_string(),
                "-U".to_string(), u.username().to_string(),
                "-d".to_string(), u.path().trim_start_matches('/').to_string(),
            ])
            .envs(env.clone())
            .env_remove("VAULTKEEPER_MASTER_KEY")
            .env_remove("RESTIC_PASSWORD");
            let out = crate::util::output_with_timeout(&mut cmd, super::timeout_from_settings(&ctx.settings))?;
            anyhow::ensure!(out.status.success(), "psql query failed");
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        };
        psql("ANALYZE")?;
        let tables: i64 = psql("SELECT count(*) FROM information_schema.tables WHERE table_schema = 'public'")?.parse()?;
        let rows: i64 = psql("SELECT coalesce(sum(n_live_tup),0)::bigint FROM pg_stat_user_tables")?.parse()?;
        anyhow::ensure!(tables > 0, "verify restored zero tables");
        Ok(format!("tables={tables} approx_rows={rows}"))
    }
```

`src/engines/mongodb.rs` verify + parser:

```rust
pub fn parse_restored_docs(out: &str) -> Option<u64> {
    for line in out.lines() {
        if let Some(idx) = line.find(" document(s) restored successfully") {
            let head = &line[..idx];
            let num = head.rsplit(|c: char| !c.is_ascii_digit()).next()?;
            let num = head[head.len() - num.len()..].parse().ok()?;
            return Some(num);
        }
    }
    None
}
```

(Implementer note: the digit extraction above is intentionally simple: take the trailing run of ascii digits from the text before the marker. If the borrow gymnastics fight you, a 5-line loop collecting trailing digits is fine; keep the function signature and tests exactly.)

```rust
    fn verify(&self, ctx: &VerifyCtx) -> Result<String> {
        let scratch = ctx
            .scratch_mongodb
            .as_deref()
            .context("mongodb verify needs a scratch database: configure [verify] mongodb_uri")?;
        let payload = crate::util::find_named(&ctx.restored_dir, &ctx.source_name)?;
        let config_path = payload.join(".mongorestore-config.yml");
        crate::util::write_new_0600(&config_path, format!("uri: {scratch}\n").as_bytes())?;
        let mut cmd = std::process::Command::new("mongorestore");
        cmd.args([
            "--config".to_string(), config_path.display().to_string(),
            "--drop".to_string(), "--dir".to_string(), payload.join("dump").display().to_string(),
        ])
        .env_remove("VAULTKEEPER_MASTER_KEY")
        .env_remove("RESTIC_PASSWORD");
        let out = crate::util::output_with_timeout(&mut cmd, super::timeout_from_settings(&ctx.settings));
        let _ = std::fs::remove_file(&config_path);
        if config_path.exists() {
            anyhow::bail!("mongorestore config file could not be removed; aborting");
        }
        let out = out?;
        let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        if !out.status.success() {
            anyhow::bail!("verify mongorestore failed: {}", crate::util::truncate_marked(&combined, 2000));
        }
        let docs = parse_restored_docs(&combined).context("could not parse restored document count")?;
        anyhow::ensure!(docs > 0, "verify restored zero documents");
        Ok(format!("docs={docs}"))
    }
```

`src/engines/supabase_storage.rs` verify:

```rust
    fn verify(&self, ctx: &VerifyCtx) -> Result<String> {
        let mirror = crate::util::find_named(&ctx.restored_dir, &ctx.source_name)?;
        let (files, bytes) = crate::util::dir_stats(&mirror)?;
        anyhow::ensure!(files > 0, "verify found zero files in the restored mirror");
        Ok(format!("files={files} bytes={bytes}"))
    }
```

`src/engines/supabase_functions.rs` verify:

```rust
    fn verify(&self, ctx: &VerifyCtx) -> Result<String> {
        let payload = crate::util::find_named(&ctx.restored_dir, &ctx.source_name)?;
        let fns_dir = payload.join("supabase").join("functions");
        let count = std::fs::read_dir(&fns_dir)
            .with_context(|| format!("no functions directory in restored snapshot at {}", fns_dir.display()))?
            .count();
        anyhow::ensure!(count > 0, "verify found zero functions");
        anyhow::ensure!(payload.join("auth-config.json").exists(), "auth-config.json missing from snapshot");
        Ok(format!("functions={count} auth_config=present"))
    }
```

- [ ] **Step 4: Run to verify pass**

Run: full gate.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: engine verify with scratch databases and metrics"
```

---

### Task 6: exec::execute_verify, exec::execute_restore, CLI wiring

**Files:**
- Modify: `src/exec.rs`, `src/main.rs`
- Test: inline in `src/exec.rs` (pure helper) plus a tests/cli.rs case

**Interfaces:**
- Consumes: everything above.
- Produces:
  - `exec::restore_workdir(staging_dir: &Path, kind: &str, name: &str) -> PathBuf` (pure: `<staging>/.{kind}/<name>`, tested).
  - `exec::execute_verify(cfg: &config::Config, db_path: &str, source_name: &str) -> anyhow::Result<pipeline::RunOutcome>`: opens store, `start_run(sid, "verify")` (guard applies), `latest_snapshot`, wipes+creates the workdir (0700 unix), `repo.restore`, builds `VerifyCtx` (scratch URLs from `cfg.verify`), `engine.verify`; `finish_run` with `verify_passed` + metrics detail or `verify_failed` + error detail; sends `RunEvent::Finished` (NO Started ping for verifies); wipes the workdir after; returns the outcome. A start_run guard refusal or pre-start failure returns Err without a Finished notification (matches backup semantics).
  - `exec::execute_restore(cfg, db_path, source_name, snapshot: Option<&str>, target: Option<&str>, force_same_host: bool, confirm_remote_overwrite: bool) -> anyhow::Result<()>`: `start_run(sid, "restore")`, resolve snapshot (given id or `latest_snapshot`), restic restore into the workdir, `engine.restore(RestoreCtx)`, `finish_run` success/failed with detail, wipe workdir. No notifications (restores are operator-interactive; documented in a comment).
  - CLI: `vaultkeeper restore --source N [--snapshot ID] [--target URL] [--force-same-host] [--confirm-remote-overwrite]`; `vaultkeeper verify --source N` (verify-all-sources arrives with the scheduler task); both print the outcome.

- [ ] **Step 1: Write the failing tests**

`src/exec.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_workdir_layout() {
        let d = restore_workdir(std::path::Path::new("/staging"), "verify", "acme-db");
        assert!(d.ends_with(std::path::Path::new(".verify/acme-db")));
        let r = restore_workdir(std::path::Path::new("/staging"), "restore", "acme-db");
        assert!(r.ends_with(std::path::Path::new(".restore/acme-db")));
    }
}
```

`tests/cli.rs`: `restore_requires_known_source`: run `restore --source ghost --target postgres://u:p@x.example.com/db` with temp db/config; assert failure and stderr contains `ghost`. (No restic needed: get_source fails before any repo call. Confirm that ordering in the implementation.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test exec`
Expected: compile error, helpers missing.

- [ ] **Step 3: Implement**

`src/exec.rs` additions (execute_source unchanged):

```rust
pub fn restore_workdir(staging_dir: &std::path::Path, kind: &str, name: &str) -> std::path::PathBuf {
    staging_dir.join(format!(".{kind}")).join(name)
}

fn fresh_workdir(dir: &std::path::Path) -> Result<()> {
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn build_repo(cfg: &config::Config) -> restic::ResticCli {
    let mut repo = restic::ResticCli::new(cfg.global.restic_repo.clone(), cfg.global.restic_password.clone());
    if let Some(mins) = cfg.global.restic_timeout_minutes {
        repo = repo.with_timeout(std::time::Duration::from_secs(mins.saturating_mul(60)));
    }
    repo
}

pub fn execute_verify(cfg: &config::Config, db_path: &str, source_name: &str) -> Result<pipeline::RunOutcome> {
    let st = store::Store::open(db_path, crypto::MasterKey::from_env()?)?;
    let source = st.get_source(source_name)?;
    let engine = engines::engine_for(&source.engine)?;
    let repo = build_repo(cfg);
    let notifier = Notifier::from_notify(&cfg.notify)?;
    let run_id = st.start_run(source.id, "verify")?;

    let workdir = restore_workdir(&cfg.global.staging_dir, "verify", &source.name);
    let result = (|| -> Result<String> {
        use crate::restic::Repo as _;
        repo.ensure_init()?;
        let snap = restic::latest_snapshot(&repo, &format!("source={}", source.name))?;
        fresh_workdir(&workdir)?;
        repo.restore(&snap.id, &workdir)?;
        let ctx = engines::VerifyCtx {
            restored_dir: workdir.clone(),
            source_name: source.name.clone(),
            scratch_postgres: cfg.verify.postgres_url.clone(),
            scratch_mongodb: cfg.verify.mongodb_uri.clone(),
            settings: source.settings.clone(),
            secrets: source.secrets.clone(),
        };
        engine.verify(&ctx)
    })();
    let _ = std::fs::remove_dir_all(&workdir);

    let (status, detail) = match &result {
        Ok(metrics) => ("verify_passed", metrics.clone()),
        Err(e) => ("verify_failed", crate::util::truncate_marked(&format!("{e:#}"), 4000)),
    };
    if let Err(je) = st.finish_run(run_id, status, None, None, Some(&detail)) {
        tracing::warn!("failed to journal verify run {run_id}: {je:#}");
    }
    notifier.notify(
        &source.name,
        source.healthchecks_uuid.as_deref(),
        &RunEvent::Finished { status, snapshot_id: None, detail: Some(&detail) },
    );
    result.map(|_| pipeline::RunOutcome { run_id, snapshot_id: None, status: status.into() })
}

#[allow(clippy::too_many_arguments)]
pub fn execute_restore(
    cfg: &config::Config,
    db_path: &str,
    source_name: &str,
    snapshot: Option<&str>,
    target: Option<&str>,
    force_same_host: bool,
    confirm_remote_overwrite: bool,
) -> Result<()> {
    let st = store::Store::open(db_path, crypto::MasterKey::from_env()?)?;
    let source = st.get_source(source_name)?;
    let engine = engines::engine_for(&source.engine)?;
    let repo = build_repo(cfg);
    let run_id = st.start_run(source.id, "restore")?;

    let workdir = restore_workdir(&cfg.global.staging_dir, "restore", &source.name);
    // Restores are operator-driven and interactive: outcomes are journaled
    // but not sent to notification channels.
    let result = (|| -> Result<()> {
        use crate::restic::Repo as _;
        repo.ensure_init()?;
        let snap_id = match snapshot {
            Some(id) => id.to_string(),
            None => restic::latest_snapshot(&repo, &format!("source={}", source.name))?.id,
        };
        fresh_workdir(&workdir)?;
        repo.restore(&snap_id, &workdir)?;
        let ctx = engines::RestoreCtx {
            restored_dir: workdir.clone(),
            source_name: source.name.clone(),
            target: target.map(|t| t.to_string()),
            force_same_host,
            confirm_remote_overwrite,
            settings: source.settings.clone(),
            secrets: source.secrets.clone(),
        };
        engine.restore(&ctx)
    })();
    let _ = std::fs::remove_dir_all(&workdir);

    match &result {
        Ok(()) => {
            if let Err(je) = st.finish_run(run_id, "success", None, None, None) {
                tracing::warn!("failed to journal restore run {run_id}: {je:#}");
            }
        }
        Err(e) => {
            let detail = crate::util::truncate_marked(&format!("{e:#}"), 4000);
            if let Err(je) = st.finish_run(run_id, "failed", None, None, Some(&detail)) {
                tracing::warn!("failed to journal restore run {run_id}: {je:#}");
            }
        }
    }
    result
}
```

Also refactor `execute_source` to use `build_repo` (removes the triplicated override noted in the ledger; scheduler keeps its own copy for boot fail-fast or may also call `exec::build_repo` if visibility allows: make `build_repo` `pub(crate)` and use it from scheduler.rs too).

`src/main.rs`: add subcommands:

```rust
    /// Restore a snapshot into a target database
    Restore {
        #[arg(long)]
        source: String,
        #[arg(long)]
        snapshot: Option<String>,
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        force_same_host: bool,
        #[arg(long)]
        confirm_remote_overwrite: bool,
    },
    /// Restore the latest snapshot into scratch databases and check it
    Verify {
        #[arg(long)]
        source: String,
    },
```

with arms delegating to `exec::execute_restore` / `exec::execute_verify` and printing the outcome status (verify prints the metrics detail line on success).

- [ ] **Step 4: Run to verify pass**

Run: full gate, including the new tests/cli.rs case.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: restore and verify commands with journaled outcomes"
```

---

### Task 7: Scheduled verifies in the daemon

**Files:**
- Modify: `src/scheduler.rs`
- Test: inline (projection helper)

**Interfaces:**
- Consumes: `exec::execute_verify`, existing daemon structure.
- Produces: the daemon spawns a SECOND task for every enabled source that has a `verify_schedule`, structurally identical to the backup task but calling `execute_verify`. The source projection becomes `Vec<(String, String, Option<String>)>` (name, schedule, verify_schedule); startup validates verify schedules too (already validated in check-config; validate here as well before spawning). Pure helper `verify_jobs(sources: &[(String, String, Option<String>)]) -> Vec<(String, String)>` returns (name, verify_schedule) pairs, tested.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn verify_jobs_filters_sources_with_verify_schedules() {
        let sources = vec![
            ("a".to_string(), "0 2 * * *".to_string(), Some("0 5 * * 0".to_string())),
            ("b".to_string(), "0 3 * * *".to_string(), None),
        ];
        assert_eq!(verify_jobs(&sources), vec![("a".to_string(), "0 5 * * 0".to_string())]);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test scheduler`
Expected: compile error.

- [ ] **Step 3: Implement**

```rust
pub fn verify_jobs(sources: &[(String, String, Option<String>)]) -> Vec<(String, String)> {
    sources
        .iter()
        .filter_map(|(name, _, vs)| vs.as_ref().map(|v| (name.clone(), v.clone())))
        .collect()
}
```

Projection gains `s.verify_schedule`; validation loop validates both expressions; after spawning the backup tasks, iterate `verify_jobs(&sources)` spawning the same loop shape with `exec::execute_verify` inside `spawn_blocking` and log lines saying `verify` (e.g. `"{name}: next verify at {next}"`). The startup log line reports both counts: `"daemon starting with N source(s), M scheduled verif(y/ies); source changes require a restart"`.

- [ ] **Step 4: Run to verify pass**

Run: full gate.

- [ ] **Step 5: Commit**

```bash
git add src/scheduler.rs
git commit -m "feat: scheduled verify runs in the daemon"
```

---

### Task 8: e2e restore roundtrip in CI, README, check-config

**Files:**
- Modify: `tests/e2e_restic.rs`, `README.md`, `src/main.rs`

**Interfaces:**
- Consumes: the full restore path with real restic.
- Produces: the CI e2e, after the existing backup assertions, creates a fake `pg_restore` shim (writes a marker file recording its argv, exits 0) and a fake `psql` shim (echoes `1` for any -Atc query), runs `vaultkeeper restore --source e2e-db --target postgres://u:pw@elsewhere.example.com:5432/restored`, asserts exit success and that the pg_restore marker exists and its recorded argv contains `--clean` and `-d restored` and does NOT contain `pw`. README roadmap line `- [x] Restore command + scheduled restore verification`. check-config prints `verify: postgres scratch configured` / `mongodb scratch configured` / `verify: no scratch databases configured` (names only).

- [ ] **Step 1: Extend the e2e test**

Append to `full_backup_into_local_restic_repo` after the snapshots assertions (same run/shim conventions as the existing code; unix shim scripts, `#[cfg(unix)]` chmod):

```rust
    // restore roundtrip with shims: pg_restore records argv, psql answers queries
    let pg_restore = shim.join("pg_restore");
    std::fs::write(
        &pg_restore,
        "#!/bin/sh\necho \"$@\" > \"$SHIM_MARKER\"\nexit 0\n",
    )
    .unwrap();
    let psql = shim.join("psql");
    std::fs::write(&psql, "#!/bin/sh\necho 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&pg_restore, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&psql, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let marker = dir.path().join("pg_restore_argv.txt");
    let out = Command::new(env!("CARGO_BIN_EXE_vaultkeeper"))
        .env("VAULTKEEPER_MASTER_KEY", K)
        .env("VAULTKEEPER_DB", &db)
        .env("VAULTKEEPER_CONFIG", &cfg_path)
        .env("PATH", &path_env)
        .env("SHIM_MARKER", &marker)
        .args([
            "restore", "--source", "e2e-db",
            "--target", "postgres://u:pw@elsewhere.example.com:5432/restored",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "restore failed: {}", String::from_utf8_lossy(&out.stderr));
    let argv = std::fs::read_to_string(&marker).unwrap();
    assert!(argv.contains("--clean"));
    assert!(argv.contains("-d restored"));
    assert!(!argv.contains("pw"), "password must never reach pg_restore argv");
```

- [ ] **Step 2: Run locally to confirm still ignored**

Run: `cargo test --test e2e_restic`
Expected: `1 ignored` (unchanged locally; executes in CI).

- [ ] **Step 3: README + check-config**

README: check the restore/verify roadmap line. `src/main.rs` CheckConfig arm, after the notify block:

```rust
            let mut scratch = Vec::new();
            if cfg.verify.postgres_url.is_some() {
                scratch.push("postgres scratch configured");
            }
            if cfg.verify.mongodb_uri.is_some() {
                scratch.push("mongodb scratch configured");
            }
            if scratch.is_empty() {
                println!("verify: no scratch databases configured");
            } else {
                println!("verify: {}", scratch.join(", "));
            }
```

- [ ] **Step 4: Full gate, commit, push is controller-handled**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`

```bash
git add -A
git commit -m "feat: restore e2e roundtrip, verify config surfacing, roadmap"
```

---

## Self-Review Notes

- Spec coverage: restore command with snapshot selection and same-host guard (Tasks 3, 4, 6), storage overwrite confirmation (Task 4), functions manual-steps restore (Task 4), verify into scratch DBs with row counts journaled and notified (Tasks 5, 6), scheduled verifies via verify_schedule (Task 7), storage/functions structural verifies per spec (Task 5). Mandates: running guard + repo-failure journaling + journal-failure test (Task 1), fail-closed statuses (Task 2). Compose verify profile is plan 5.
- Type consistency: RestoreCtx/VerifyCtx defined once in engines/mod.rs and used identically across Tasks 4, 5, 6; `restore_workdir`/`build_repo` shared inside exec.rs; statuses match Task 2's lists.
- Placeholder scan: all code steps carry complete code; the psql/mongorestore child legs follow the established untested-glue pattern with pure parsers tested.
- Known simplification consciously accepted: verify journals bytes/snapshot_id as None (metrics live in detail); `verify --source` is per-source only until the TUI arrives.
