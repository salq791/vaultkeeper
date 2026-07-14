# Vaultkeeper Plan 5: Shippable Container + Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `docker compose up -d` runs vaultkeeper as designed (one container with every native tool, SIGTERM-graceful, non-root), the image publishes to GHCR from CI, and a compose-based CI smoke proves the full product loop (backup, verify, restore) against REAL postgres, mongo, and S3 services.

**Architecture:** Two hardening tasks clear the mandated backlog (SIGTERM + boot stale-run reconciliation + WAL + success-path journal guard; child env scrubbing + verify strictness + guard-skip journaling + restore target hygiene + check-config exit codes). Then the deployment surface: a multi-stage Dockerfile (rust builder, debian-slim runtime with pgdg postgresql-client-18, mongodb-database-tools, restic, rclone, supabase CLI, non-root uid 1000), docker-compose.yml with a `verify` profile providing scratch databases, a GHCR publish job, and `scripts/smoke.sh` driving a dedicated smoke compose file in CI: three sources (postgres, mongo, minio-S3) through add, run, snapshots, verify (with a percent-encoded scratch password and sslmode param, per review mandate), and a postgres restore leg asserted via psql.

**Tech Stack:** No new Rust dependencies. Docker/buildx, docker compose v2, GHCR via GITHUB_TOKEN, postgres:18-alpine, mongo:8, minio/minio.

**Spec:** `docs/superpowers/specs/2026-07-13-vaultkeeper-design.md` (Docker section). Plan 5 of 6 (plan 6 = ratatui TUI + launch docs). The MANDATORY items from the plan-3/plan-4 final reviews are enumerated per task below; the ledger copy lives in `.superpowers/sdd/progress.md`.

## Global Constraints

- PUBLIC REPO: no secrets, tokens, real hostnames, or real project refs in ANY committed file. Smoke fixtures use synthetic values (sourcepw, smokeadmin, p@ss/word) that exist only inside throwaway CI containers.
- Never use em dashes in any file, code comment, doc, Dockerfile, yaml, or shell script. Use commas, colons, or hyphens.
- `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` must pass at every commit.
- GitHub Actions are SHA-pinned with a trailing version comment (repo convention); any new action must be pinned the same way (resolve via `git ls-remote`).
- The container runs as non-root uid 1000 (`vaultkeeper`), entrypoint `vaultkeeper`, default command `daemon`. STOPSIGNAL stays default SIGTERM because Task 1 makes SIGTERM graceful.
- Conventional commit messages with trailer: Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
- TDD with REAL captured RED output for every Rust change; Docker/compose/smoke artifacts are verified by the CI smoke job (they cannot run on the Windows dev machine; disclose that plainly in reports).
- Status strings gain `skipped` (guard-refused scheduled runs). Full set: success, success_prune_failed, failed, verify_passed, verify_failed, stale, skipped, running.

---

### Task 1: SIGTERM, boot reconciliation, WAL, success-path journal guard

**Files:**
- Modify: `src/scheduler.rs`, `src/store.rs`, `src/pipeline.rs`
- Test: inline in `src/store.rs`

**Interfaces:**
- Consumes: existing daemon/store/pipeline.
- Produces: `store::Store::reconcile_stale_running(&self) -> anyhow::Result<u64>` (marks EVERY `running` row `stale` with finished_at, returns count; the daemon owns the DB at boot, so anything still `running` is a crashed process's zombie); daemon shutdown triggers on SIGTERM as well as ctrl-c (unix), so `docker stop` is graceful; `Store::open` sets `PRAGMA journal_mode=WAL` on file-backed databases; pipeline success and success_prune_failed arms warn-guard `finish_run` failures instead of `?` (a real snapshot must never be reported as run failure because the journal write failed; mirrors the failed arm's existing guard).

- [ ] **Step 1: Write the failing tests**

`src/store.rs` tests:

```rust
    #[test]
    fn reconcile_marks_all_running_rows_stale() {
        let st = store();
        let sid = st.add_source(&sample()).unwrap();
        let _r = st.start_run(sid, "backup").unwrap();
        assert_eq!(st.reconcile_stale_running().unwrap(), 1);
        let stale: i64 = st
            .conn_for_tests()
            .query_row("SELECT count(*) FROM runs WHERE status = 'stale'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stale, 1);
        assert!(st.start_run(sid, "backup").is_ok(), "reconciled source is unblocked");
    }

    #[test]
    fn file_backed_store_uses_wal() {
        let dir = tempfile::tempdir().unwrap();
        let st = Store::open(dir.path().join("w.db").to_str().unwrap(), MasterKey::from_hex(K).unwrap()).unwrap();
        let mode: String = st
            .conn_for_tests()
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test store`
Expected: compile error, `reconcile_stale_running` missing; WAL test fails (journal_mode is delete).

- [ ] **Step 3: Implement**

`src/store.rs`:

```rust
    /// Marks every 'running' row stale. Called once at daemon boot: the
    /// daemon owns the database, so any row still 'running' at that moment
    /// belongs to a process that died without finishing its journal entry.
    pub fn reconcile_stale_running(&self) -> Result<u64> {
        let n = self.conn.execute(
            "UPDATE runs SET status = 'stale', finished_at = datetime('now') WHERE status = 'running'",
            [],
        )?;
        Ok(n as u64)
    }
```

In `Store::open`, after the busy_timeout line (the pragma returns a row, so use query_row and ignore the value; on :memory: databases SQLite reports `memory` and that is fine):

```rust
        let _: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
```

`src/scheduler.rs`: in `run_daemon`, right after opening the store and before listing sources:

```rust
    let cleared = st.reconcile_stale_running()?;
    if cleared > 0 {
        tracing::warn!("cleared {cleared} zombie 'running' row(s) from a previous process");
    }
```

Replace the `tokio::signal::ctrl_c().await` line with a shutdown that also honors SIGTERM (what `docker stop` sends):

```rust
    shutdown_signal().await;
```

and add:

```rust
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    tracing::info!("shutdown signal received: stopping schedules, waiting for in-flight runs");
}
```

(Remove the old context/expect on ctrl_c and the now-duplicated log line at the old call site.)

`src/pipeline.rs`: both Ok arms replace `store.finish_run(...)?;` with the failed arm's guard pattern:

```rust
            if let Err(journal_err) = store.finish_run(run_id, "success", Some(bytes), Some(&snapshot_id), None) {
                tracing::warn!("failed to journal run {run_id} success: {journal_err:#}");
            }
```

(and the same shape for `success_prune_failed` with its detail argument).

- [ ] **Step 4: Run to verify pass**

Run: full gate. tokio's `signal` feature is already enabled (plan 3).

- [ ] **Step 5: Commit**

```bash
git add src/store.rs src/scheduler.rs src/pipeline.rs
git commit -m "feat: sigterm shutdown, boot zombie reconciliation, wal, journal guards"
```

---

### Task 2: Child env scrubbing, verify strictness, guard-skip journaling, restore hygiene, check-config exit codes

**Files:**
- Modify: `src/engines/mod.rs`, all four engine files, `src/engines/mongodb.rs`, `src/store.rs`, `src/exec.rs`, `src/main.rs`
- Test: inline in `src/engines/mod.rs`, `src/engines/mongodb.rs`, `src/store.rs`; updates in `tests/cli.rs`

**Interfaces:**
- Consumes: existing engines/exec/main.
- Produces:
  - `engines::SCRUBBED_ENV_VARS: [&str; 6]` = `["VAULTKEEPER_MASTER_KEY", "RESTIC_PASSWORD", "AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "AWS_SESSION_TOKEN", "AWS_PROFILE"]` and `engines::scrub_child_env(cmd: &mut std::process::Command)` applying env_remove for each. EVERY engine child (pg_dump, pg_restore, psql, mongodump, mongorestore, rclone, supabase) calls it, replacing the inline pairs. Restic's own child keeps ONLY its inline `env_remove("VAULTKEEPER_MASTER_KEY")` (restic may legitimately need AWS creds for S3 repos).
  - `mongodb::parse_failed_docs(out: &str) -> Option<u64>` (parses "N document(s) failed to restore"); mongodb verify additionally fails when the failed count parses to > 0.
  - `store::Store::record_skip(&self, source_id: i64, kind: &str, reason: &str) -> anyhow::Result<()>` inserting a row with status `skipped` and finished_at set; `exec::execute_verify` calls it when `start_run` refuses with the in-progress guard (so silent verify skips become journal-visible).
  - `main.rs` Restore arm: `--target` may be omitted and falls back to env `VAULTKEEPER_RESTORE_TARGET`; when `--target` IS passed inline, print the established stderr warning style: `warning: inline --target exposes the database password to the process table and shell history; prefer VAULTKEEPER_RESTORE_TARGET`.
  - check-config exits nonzero when any schedule is INVALID or any tool is MISSING: count problems, end with `anyhow::ensure!(problems == 0, "check-config found {problems} problem(s)")`.
  - `mongodb::uri_host` gains a doc note: returns None for URIs without userinfo (no '@'), so same-host guards fail open for credential-less URIs by design; the credential-bearing case is the guarded one.

- [ ] **Step 1: Write the failing tests**

`src/engines/mod.rs`:

```rust
    #[test]
    fn scrub_list_covers_vault_and_aws() {
        for var in ["VAULTKEEPER_MASTER_KEY", "RESTIC_PASSWORD", "AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "AWS_SESSION_TOKEN", "AWS_PROFILE"] {
            assert!(SCRUBBED_ENV_VARS.contains(&var), "{var} must be scrubbed");
        }
    }
```

`src/engines/mongodb.rs`:

```rust
    #[test]
    fn parses_mongorestore_failed_count() {
        let out = "55 document(s) restored successfully. 5 document(s) failed to restore.";
        assert_eq!(parse_failed_docs(out), Some(5));
        assert_eq!(parse_failed_docs("55 document(s) restored successfully. 0 document(s) failed to restore."), Some(0));
        assert_eq!(parse_failed_docs("nothing here"), None);
    }
```

`src/store.rs`:

```rust
    #[test]
    fn record_skip_writes_finished_skipped_row() {
        let st = store();
        let sid = st.add_source(&sample()).unwrap();
        st.record_skip(sid, "verify", "another run in progress").unwrap();
        let runs = st.recent_runs(1).unwrap();
        assert_eq!(runs[0].status, "skipped");
        assert_eq!(runs[0].kind, "verify");
        assert!(runs[0].finished_at.is_some());
        assert!(runs[0].detail.as_deref().unwrap().contains("in progress"));
    }
```

`tests/cli.rs`: new test `check_config_fails_on_missing_tools`: temp config as in the existing check-config test, but run with `PATH` set to an empty temp dir; assert the command FAILS and stdout contains "MISSING". Also extend the restore ghost test: run with env `VAULTKEEPER_RESTORE_TARGET=postgres://u:p@elsewhere.example.com:5432/db` and NO `--target`; assert it still fails with "ghost" (proves the env fallback wires through before source lookup errors).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test`
Expected: compile errors for the three new symbols; CLI test fails (check-config currently exits 0 with missing tools).

- [ ] **Step 3: Implement**

`src/engines/mod.rs`:

```rust
/// Env vars scrubbed from every engine child. Restic is the one exception
/// for the AWS vars: an S3-backed restic repo legitimately needs them.
pub const SCRUBBED_ENV_VARS: [&str; 6] = [
    "VAULTKEEPER_MASTER_KEY",
    "RESTIC_PASSWORD",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_PROFILE",
];

pub fn scrub_child_env(cmd: &mut std::process::Command) {
    for var in SCRUBBED_ENV_VARS {
        cmd.env_remove(var);
    }
}
```

In each engine file, replace the chained `.env_remove("VAULTKEEPER_MASTER_KEY").env_remove("RESTIC_PASSWORD")` on pg_dump/pg_restore/psql/mongodump/mongorestore/rclone/supabase commands with a `super::scrub_child_env(&mut cmd);` call after the command is otherwise built. Do NOT touch `src/restic.rs`.

`src/engines/mongodb.rs`:

```rust
pub fn parse_failed_docs(out: &str) -> Option<u64> {
    for line in out.lines() {
        if let Some(idx) = line.find(" document(s) failed to restore") {
            let head = &line[..idx];
            let digits: String = head.chars().rev().take_while(|c| c.is_ascii_digit()).collect();
            let digits: String = digits.chars().rev().collect();
            return digits.parse().ok();
        }
    }
    None
}
```

and in `verify()` after the success/doc-count checks:

```rust
        if let Some(failed) = parse_failed_docs(&combined) {
            anyhow::ensure!(failed == 0, "verify mongorestore reported {failed} failed document(s)");
        }
```

`src/store.rs`:

```rust
    /// Journals a run that was refused by the concurrency guard so scheduled
    /// skips are visible in history instead of only in daemon logs.
    pub fn record_skip(&self, source_id: i64, kind: &str, reason: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO runs (source_id, kind, status, finished_at, detail)
             VALUES (?1, ?2, 'skipped', datetime('now'), ?3)",
            params![source_id, kind, reason],
        )?;
        Ok(())
    }
```

`src/exec.rs` in `execute_verify`, replace `let run_id = st.start_run(source.id, "verify")?;` with:

```rust
    let run_id = match st.start_run(source.id, "verify") {
        Ok(id) => id,
        Err(e) => {
            if e.to_string().contains("in progress") {
                let _ = st.record_skip(source.id, "verify", &e.to_string());
            }
            return Err(e);
        }
    };
```

`src/main.rs` Restore arm, before calling execute_restore:

```rust
            let target = match target {
                Some(t) => {
                    eprintln!(
                        "warning: inline --target exposes the database password to the process table and shell history; prefer VAULTKEEPER_RESTORE_TARGET"
                    );
                    Some(t)
                }
                None => std::env::var("VAULTKEEPER_RESTORE_TARGET").ok(),
            };
```

check-config: `let mut problems = 0usize;` incremented at every `INVALID` schedule print and every `MISSING from PATH` tool print; after the verify-scratch block:

```rust
            anyhow::ensure!(problems == 0, "check-config found {problems} problem(s)");
            Ok(())
```

`RunRow.finished_at` is read by the new store test; if it still carries `#[allow(dead_code)]` from earlier plans, remove what clippy no longer needs.

- [ ] **Step 4: Run to verify pass**

Run: full gate including tests/cli.rs (the existing `check_config_reports_notify_channels` test keeps passing because its PATH still finds no INVALID schedules but tools may be MISSING on dev machines; ADAPT that existing test in the same commit: it now asserts only the notify line while tolerating nonzero exit, OR points PATH at a dir containing stub executables. Choose the first: change its `assert!(out.status.success())` to drop the exit assertion and keep the stdout assertion, with a comment referencing the new dedicated exit-code test.)

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: env scrubbing, verify strictness, skip journaling, config exit codes"
```

---

### Task 3: Dockerfile + .dockerignore

**Files:**
- Create: `Dockerfile`, `.dockerignore`

**Interfaces:**
- Consumes: the release binary.
- Produces: image with `vaultkeeper` plus restic, rclone, pg_dump/pg_restore/psql (postgresql-client-18 from pgdg), mongodump/mongorestore (mongodb-database-tools 8.0 repo), supabase CLI; non-root uid 1000; volumes `/config`, `/data`, `/staging`; entrypoint `vaultkeeper`, default cmd `daemon`. Task 6's smoke builds this exact file.

- [ ] **Step 1: Write the files**

`.dockerignore`:

```
target
.git
.superpowers
docs
.env
```

`Dockerfile`:

```dockerfile
FROM rust:1-bookworm AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY tests ./tests
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl gnupg \
    && install -d /usr/share/postgresql-common/pgdg \
    && curl -fsSL -o /usr/share/postgresql-common/pgdg/apt.postgresql.org.asc https://www.postgresql.org/media/keys/ACCC4CF8.asc \
    && echo "deb [signed-by=/usr/share/postgresql-common/pgdg/apt.postgresql.org.asc] https://apt.postgresql.org/pub/repos/apt bookworm-pgdg main" > /etc/apt/sources.list.d/pgdg.list \
    && curl -fsSL https://www.mongodb.org/static/pgp/server-8.0.asc | gpg --dearmor -o /usr/share/keyrings/mongodb-server-8.0.gpg \
    && echo "deb [signed-by=/usr/share/keyrings/mongodb-server-8.0.gpg] http://repo.mongodb.org/apt/debian bookworm/mongodb-org/8.0 main" > /etc/apt/sources.list.d/mongodb-org-8.0.list \
    && apt-get update \
    && apt-get install -y --no-install-recommends postgresql-client-18 mongodb-database-tools restic rclone \
    && curl -fsSL -o /tmp/supabase.deb https://github.com/supabase/cli/releases/latest/download/supabase_linux_amd64.deb \
    && apt-get install -y --no-install-recommends /tmp/supabase.deb \
    && rm -f /tmp/supabase.deb \
    && apt-get purge -y gnupg \
    && apt-get autoremove -y \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /src/target/release/vaultkeeper /usr/local/bin/vaultkeeper

RUN useradd --uid 1000 --create-home vaultkeeper \
    && mkdir -p /config /data /staging \
    && chown vaultkeeper:vaultkeeper /data /staging

USER vaultkeeper
VOLUME ["/data", "/staging"]
ENTRYPOINT ["/usr/local/bin/vaultkeeper"]
CMD ["daemon"]
```

(Implementer notes, pre-authorized adaptations to disclose: if `postgresql-client-18` is not yet in pgdg for bookworm, use the highest available major; if the supabase deb URL shape changed, use the current documented release asset; keep amd64, arm64 is a plan-6 ledger note.)

- [ ] **Step 2: Sanity-check the Rust build stage compiles the same binary**

Run: `cargo build --release` locally (Windows binary, but proves the manifest paths in the COPY lines cover everything the build needs: Cargo.toml, Cargo.lock, src, tests).
Expected: builds clean.

- [ ] **Step 3: Commit**

```bash
git add Dockerfile .dockerignore
git commit -m "feat: production container image with all native tools"
```

(The image itself is built and exercised in CI by Tasks 5 and 6; it cannot be built on this Windows machine without Docker Desktop assumptions, disclose in the report.)

---

### Task 4: docker-compose.yml + config example

**Files:**
- Create: `docker-compose.yml`, `config.example.toml`
- Modify: `.env.example`

**Interfaces:**
- Consumes: the image (built locally via `build: .` or pulled from GHCR).
- Produces: the deployment users copy: `vaultkeeper` service plus `verify-postgres`/`verify-mongo` under the `verify` profile, matching the spec's compose description and the plan-3 spec note that verify fails loudly when the profile is not started.

- [ ] **Step 1: Write the files**

`docker-compose.yml`:

```yaml
services:
  vaultkeeper:
    build: .
    image: ghcr.io/salq791/vaultkeeper:latest
    restart: unless-stopped
    env_file: .env
    environment:
      VAULTKEEPER_CONFIG: /config/config.toml
      VAULTKEEPER_DB: /data/vaultkeeper.db
    volumes:
      - ./config.toml:/config/config.toml:ro
      - vk-data:/data
      - vk-staging:/staging

  verify-postgres:
    image: postgres:18-alpine
    profiles: ["verify"]
    restart: unless-stopped
    environment:
      POSTGRES_USER: verifier
      POSTGRES_PASSWORD: ${VERIFY_PG_PASSWORD:?set in .env}
      POSTGRES_DB: scratch
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U verifier -d scratch"]
      interval: 5s
      timeout: 3s
      retries: 20

  verify-mongo:
    image: mongo:8
    profiles: ["verify"]
    restart: unless-stopped
    healthcheck:
      test: ["CMD-SHELL", "mongosh --quiet --eval 'db.adminCommand({ping: 1})'"]
      interval: 5s
      timeout: 3s
      retries: 20

volumes:
  vk-data:
  vk-staging:
```

`config.example.toml`:

```toml
# Copy to config.toml next to docker-compose.yml. Secrets stay in .env.
[global]
staging_dir = "/staging"
restic_repo = "sftp:youraccount@youraccount.repo.borgbase.com:vaultkeeper"
restic_password = "${RESTIC_PASSWORD}"

[notify]
healthchecks_base = "https://hc-ping.com"
# webhook_url = "${SLACK_WEBHOOK}"
# [notify.ses]
# region = "us-east-1"
# from = "backups@example.com"
# to = ["admin@example.com"]

# Scratch databases for scheduled verify runs. Start them with:
#   docker compose --profile verify up -d
[verify]
postgres_url = "postgres://verifier:${VERIFY_PG_PASSWORD}@verify-postgres:5432/scratch?sslmode=disable"
mongodb_uri = "mongodb://verify-mongo:27017/scratch"
```

`.env.example` gains:

```
# Password for the verify-postgres scratch database (compose verify profile)
VERIFY_PG_PASSWORD=
```

- [ ] **Step 2: Validate the yaml**

Run: `python -c "import yaml; yaml.safe_load(open('docker-compose.yml'))"` (or equivalent careful review if python/yaml is unavailable; state which you did).
Expected: parses.

- [ ] **Step 3: Commit**

```bash
git add docker-compose.yml config.example.toml .env.example
git commit -m "feat: compose deployment with verify scratch profile"
```

---

### Task 5: GHCR publish job

**Files:**
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: the Dockerfile.
- Produces: `docker` job that BUILDS the image on every push/PR (catching Dockerfile rot) and PUSHES `ghcr.io/salq791/vaultkeeper:latest` + `ghcr.io/salq791/vaultkeeper:sha-<short>` only on master pushes, authenticated with GITHUB_TOKEN.

- [ ] **Step 1: Add the job**

Resolve current SHAs with `git ls-remote https://github.com/docker/login-action v3`, `git ls-remote https://github.com/docker/build-push-action v6` (dereference `^{}` entries if annotated). Append:

```yaml
  docker:
    runs-on: ubuntu-latest
    timeout-minutes: 30
    needs: [test, e2e]
    permissions:
      contents: read
      packages: write
    steps:
      - uses: actions/checkout@<pinned-sha> # v4 (same SHA as the other jobs)
      - uses: docker/login-action@<resolved-sha> # v3
        if: github.ref == 'refs/heads/master' && github.event_name == 'push'
        with:
          registry: ghcr.io
          username: ${{ github.actor }}
          password: ${{ secrets.GITHUB_TOKEN }}
      - uses: docker/build-push-action@<resolved-sha> # v6
        with:
          context: .
          push: ${{ github.ref == 'refs/heads/master' && github.event_name == 'push' }}
          tags: |
            ghcr.io/salq791/vaultkeeper:latest
            ghcr.io/salq791/vaultkeeper:sha-${{ github.sha }}
```

- [ ] **Step 2: Validate yaml, gate, commit**

Run: yaml parse check plus the full cargo gate (unchanged Rust).

```bash
git add .github/workflows/ci.yml
git commit -m "ci: build and publish container image to ghcr"
```

---

### Task 6: Compose smoke: full product loop with real tools

**Files:**
- Create: `docker-compose.smoke.yml`, `scripts/smoke.sh`, `scripts/smoke-seed.sql`, `scripts/smoke-config.toml`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: the Dockerfile image (built as `vaultkeeper:smoke`).
- Produces: CI job `smoke` proving with REAL tools: postgres backup+verify+restore (scratch password percent-encoded `p%40ss%2Fword`, `sslmode=disable` per review mandate), mongo backup+verify, minio-S3 storage backup+verify. Supabase functions leg is documented as not smokeable (needs a live project); its engine remains covered by unit tests.

- [ ] **Step 1: Write the smoke compose file**

`docker-compose.smoke.yml`:

```yaml
services:
  source-postgres:
    image: postgres:18-alpine
    environment:
      POSTGRES_PASSWORD: sourcepw
      POSTGRES_DB: app
    volumes:
      - ./scripts/smoke-seed.sql:/docker-entrypoint-initdb.d/seed.sql:ro
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U postgres -d app"]
      interval: 2s
      timeout: 3s
      retries: 30

  source-mongo:
    image: mongo:8
    healthcheck:
      test: ["CMD-SHELL", "mongosh --quiet --eval 'db.adminCommand({ping: 1})'"]
      interval: 2s
      timeout: 3s
      retries: 30

  minio:
    image: minio/minio
    command: server /data
    environment:
      MINIO_ROOT_USER: smokeadmin
      MINIO_ROOT_PASSWORD: smokesecret
    healthcheck:
      test: ["CMD-SHELL", "curl -sf http://localhost:9000/minio/health/live"]
      interval: 2s
      timeout: 3s
      retries: 30

  scratch-postgres:
    image: postgres:18-alpine
    environment:
      POSTGRES_USER: verifier
      POSTGRES_PASSWORD: p@ss/word
      POSTGRES_DB: scratch
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U verifier -d scratch"]
      interval: 2s
      timeout: 3s
      retries: 30

  scratch-mongo:
    image: mongo:8
    healthcheck:
      test: ["CMD-SHELL", "mongosh --quiet --eval 'db.adminCommand({ping: 1})'"]
      interval: 2s
      timeout: 3s
      retries: 30

  vaultkeeper:
    image: vaultkeeper:smoke
    user: "0"
    entrypoint: ["sleep", "infinity"]
    environment:
      VAULTKEEPER_CONFIG: /config/config.toml
      VAULTKEEPER_DB: /data/vaultkeeper.db
      VAULTKEEPER_MASTER_KEY: "1111111111111111111111111111111111111111111111111111111111111111"
      RESTIC_PASSWORD: smokerepopw
    volumes:
      - ./scripts/smoke-config.toml:/config/config.toml:ro
      - smoke-repo:/repo
      - smoke-staging:/staging
      - smoke-data:/data

volumes:
  smoke-repo:
  smoke-staging:
  smoke-data:
```

(`user: "0"` is smoke-only: named volumes mount root-owned and the smoke does not test unix permissions; the production compose keeps the image's non-root user. State this comment in the file.)

`scripts/smoke-seed.sql`:

```sql
CREATE TABLE items (id serial PRIMARY KEY, name text NOT NULL);
INSERT INTO items (name) VALUES ('alpha'), ('beta'), ('gamma');
```

`scripts/smoke-config.toml`:

```toml
[global]
staging_dir = "/staging"
restic_repo = "/repo"
restic_password = "${RESTIC_PASSWORD}"

[verify]
postgres_url = "postgres://verifier:p%40ss%2Fword@scratch-postgres:5432/scratch?sslmode=disable"
mongodb_uri = "mongodb://scratch-mongo:27017/scratch"
```

- [ ] **Step 2: Write the smoke script**

`scripts/smoke.sh`:

```bash
#!/usr/bin/env bash
# Full product loop against real tools: backup, snapshots, verify, restore.
set -euo pipefail
cd "$(dirname "$0")/.."
COMPOSE="docker compose -f docker-compose.smoke.yml"
VK="$COMPOSE exec -T vaultkeeper vaultkeeper"

cleanup() { $COMPOSE down -v --remove-orphans >/dev/null 2>&1 || true; }
trap cleanup EXIT

$COMPOSE up -d --wait

echo "== seed mongo =="
$COMPOSE exec -T source-mongo mongosh --quiet app --eval 'db.items.insertMany([{n:"a"},{n:"b"}])'

echo "== seed minio bucket via rclone in the vaultkeeper container =="
$COMPOSE exec -T vaultkeeper sh -c '
  export RCLONE_CONFIG_SEED_TYPE=s3 RCLONE_CONFIG_SEED_PROVIDER=Minio \
         RCLONE_CONFIG_SEED_ACCESS_KEY_ID=smokeadmin RCLONE_CONFIG_SEED_SECRET_ACCESS_KEY=smokesecret \
         RCLONE_CONFIG_SEED_ENDPOINT=http://minio:9000
  echo "object-one" > /tmp/obj1 && echo "object-two" > /tmp/obj2
  rclone mkdir SEED:smokebucket
  rclone copy /tmp/obj1 SEED:smokebucket/
  rclone copy /tmp/obj2 SEED:smokebucket/sub/
'

echo "== add sources =="
echo '{"password":"sourcepw"}' | $VK source add --name pg-src --engine postgres \
  --schedule "0 2 * * *" \
  --settings-json '{"host":"source-postgres","port":5432,"dbname":"app","user":"postgres"}' \
  --secrets-json -
echo '{"uri":"mongodb://source-mongo:27017/app"}' | $VK source add --name mongo-src --engine mongodb \
  --schedule "0 2 * * *" --settings-json '{"db":"app"}' --secrets-json -
echo '{"access_key":"smokeadmin","secret_key":"smokesecret"}' | $VK source add --name store-src \
  --engine supabase_storage --schedule "0 2 * * *" \
  --settings-json '{"endpoint":"http://minio:9000","region":"us-east-1"}' --secrets-json -

echo "== check-config must pass inside the container =="
$VK check-config

echo "== backups =="
$VK run --source pg-src
$VK run --source mongo-src
$VK run --source store-src

echo "== snapshots =="
SNAPS=$($VK snapshots)
echo "$SNAPS"
for tag in pg-src mongo-src store-src; do
  echo "$SNAPS" | grep -q "source=$tag" || { echo "missing snapshot for $tag"; exit 1; }
done

echo "== verifies (scratch password is percent-encoded, sslmode honored) =="
$VK verify --source pg-src | tee /tmp/v1 | grep -q "tables=1" || { echo "pg verify metrics wrong"; cat /tmp/v1; exit 1; }
$VK verify --source mongo-src | tee /tmp/v2 | grep -q "docs=2" || { echo "mongo verify metrics wrong"; cat /tmp/v2; exit 1; }
$VK verify --source store-src | tee /tmp/v3 | grep -q "files=2" || { echo "storage verify metrics wrong"; cat /tmp/v3; exit 1; }

echo "== restore leg: pg snapshot into a second scratch database =="
$COMPOSE exec -T scratch-postgres createdb -U verifier restored
VAULTKEEPER_RESTORE_TARGET='postgres://verifier:p%40ss%2Fword@scratch-postgres:5432/restored?sslmode=disable' \
  $COMPOSE exec -T -e VAULTKEEPER_RESTORE_TARGET vaultkeeper vaultkeeper restore --source pg-src
COUNT=$($COMPOSE exec -T scratch-postgres psql -U verifier -d restored -Atc "SELECT count(*) FROM items")
[ "$COUNT" = "3" ] || { echo "restored row count $COUNT != 3"; exit 1; }

echo "SMOKE PASSED"
```

(Note the restore leg exercises the VAULTKEEPER_RESTORE_TARGET env path from Task 2, keeping the percent-encoded password off argv, exactly the mandated end-to-end decode coverage.)

- [ ] **Step 3: Add the CI job**

Append to `.github/workflows/ci.yml`:

```yaml
  smoke:
    runs-on: ubuntu-latest
    timeout-minutes: 25
    needs: [test]
    steps:
      - uses: actions/checkout@<same-pinned-sha> # v4
      - run: docker build -t vaultkeeper:smoke .
      - run: bash scripts/smoke.sh
```

- [ ] **Step 4: Local validation, gate, commit**

Run: `bash -n scripts/smoke.sh` (syntax check; the script itself only runs in CI, disclose), yaml parse checks, full cargo gate.

```bash
git add -A
git commit -m "test: compose smoke proving backup, verify, restore with real tools"
```

---

### Task 7: README deployment section

**Files:**
- Modify: `README.md`

**Interfaces:**
- Produces: a Deploy section documenting the compose quickstart (copy config.example.toml and .env.example, `docker compose up -d`, `--profile verify` for scratch databases, `docker compose exec vaultkeeper vaultkeeper source add ...`), the GHCR image reference, and a one-line note that a terminal UI ships next. Roadmap checkboxes for scheduler/verify lines stay as-is; do not check the TUI line.

- [ ] **Step 1: Write the section**

Insert after the Roadmap section:

```markdown
## Deploy

Prebuilt image: `ghcr.io/salq791/vaultkeeper:latest` (linux/amd64).

1. Copy `config.example.toml` to `config.toml` and `.env.example` to `.env`, then fill in your restic repository, its password, and your master key (`openssl rand -hex 32`).
2. Start the daemon: `docker compose up -d`
3. Add sources (secrets via stdin so they never touch your shell history):

    echo '{"password":"..."}' | docker compose exec -T vaultkeeper \
      vaultkeeper source add --name my-db --engine postgres \
      --schedule "0 2 * * *" \
      --settings-json '{"host":"db.example.com","port":5432,"dbname":"app","user":"postgres"}' \
      --secrets-json -

4. Scheduled restore verification needs the scratch databases: `docker compose --profile verify up -d`, then add `--verify-schedule "0 5 * * 0"` to your sources.
5. `docker compose exec vaultkeeper vaultkeeper check-config` exits nonzero if anything is misconfigured.

Restores: `docker compose exec vaultkeeper vaultkeeper restore --source my-db` (target via the VAULTKEEPER_RESTORE_TARGET environment variable; same-host restores require --force-same-host).
```

- [ ] **Step 2: Gate and commit**

```bash
git add README.md
git commit -m "docs: compose deployment quickstart"
```

---

## Self-Review Notes

- Mandate coverage (ledger): SIGTERM + stale reconciliation (Task 1), WAL + success-path guard (Task 1), AWS env scrub (Task 2), mongo failed-count (Task 2), guard-skip journaling (Task 2), restore --target hygiene + env path (Task 2, exercised end-to-end by Task 6's restore leg), check-config exit codes (Task 2), uri_host fail-open note (Task 2), real-tool smoke incl. percent-encoded scratch password + sslmode (Task 6), verify e2e leg (Task 6). Deferred to plan 6 with the TUI: separate verify healthchecks UUID (needs schema/config design), SIGHUP source reload note, arm64 image.
- Type consistency: `SCRUBBED_ENV_VARS`/`scrub_child_env` names match across Tasks 2 and 6 (the smoke indirectly proves scrubbing does not break tool auth); `record_skip` signature matches its exec call; compose service names match smoke-config.toml URLs.
- Placeholder scan: `<pinned-sha>`/`<resolved-sha>` in Task 5 are explicit implementer instructions with the exact resolution commands, not placeholders left to guess.
- Consciously accepted: smoke runs containers as root (commented in the compose file); supabase functions engine has no smoke leg (documented; unit-tested); apt restic/rclone versions are bookworm's (ledger note for pinning later).
