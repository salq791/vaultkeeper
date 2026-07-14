# Vaultkeeper Plan 2: Remaining Engines + Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `vaultkeeper run --source <name>` works for all four source kinds (postgres, mongodb, supabase_storage, supabase_functions), with the final-review hardening backlog cleared.

**Architecture:** The `Engine` trait changes to return the directory restic should snapshot, enabling persistent-mirror engines (Supabase Storage syncs deltas into `<staging>/.mirrors/<name>` which survives runs) alongside wipe-per-run staging engines. New engines shell out to mongodump (secrets via a 0600 --config file, never argv), rclone (secrets via RCLONE_CONFIG_* env), and the supabase CLI (token via SUPABASE_ACCESS_TOKEN env) plus one Management API GET via reqwest.

**Tech Stack:** Existing crate plus `reqwest` (blocking, rustls, no default features). No other new dependencies; `thiserror` and `rand` are removed as unused.

**Spec:** `docs/superpowers/specs/2026-07-13-vaultkeeper-design.md`. Roadmap renumbering: this is plan 2 of 5 (3: scheduler daemon + notifications; 4: restore + verify; 5: TUI + Docker + launch docs).

## Global Constraints

- PUBLIC REPO: no secrets, tokens, real hostnames, or real project refs in ANY committed file. Only environment variable NAMES. Test fixtures use example.com domains and obviously-synthetic values.
- Never use em dashes in any file, code comment, or doc. Use commas, colons, or hyphens.
- `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` must pass at every commit.
- Secrets reach child processes via environment variables or 0600 files that are deleted after use, NEVER argv, and never appear in error messages or logs.
- Every spawned child gets `.env_remove("VAULTKEEPER_MASTER_KEY")`; children that do not need the restic password also get `.env_remove("RESTIC_PASSWORD")`.
- Tests must not require network access, real credentials, or the native tools (mongodump/rclone/supabase) installed; test the pure invocation builders.
- Conventional commit messages with trailer: Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
- TDD with REAL captured RED output in every task report; fabricated evidence fails review.
- Source names are already validated `^[A-Za-z0-9][A-Za-z0-9_-]*$` at add_source, so `.mirrors` cannot collide with a source name.

---

### Task 1: Hardening backlog from the plan-1 final review

**Files:**
- Create: `src/util.rs`
- Modify: `src/main.rs`, `src/store.rs`, `src/restic.rs`, `src/engines/postgres.rs`, `src/pipeline.rs`, `Cargo.toml`, `.github/workflows/ci.yml`, `tests/e2e_restic.rs`, `tests/cli.rs`
- Test: inline `#[cfg(test)]` in `src/util.rs`; assertions added to `tests/cli.rs`

**Interfaces:**
- Consumes: existing modules.
- Produces: `util::truncate_marked(s: &str, max_chars: usize) -> String` (appends " ...[truncated]" only when input exceeded max); `store::validate_name` becomes `pub`; restic repo moves to `RESTIC_REPOSITORY` env (constructor signature unchanged).

- [ ] **Step 1: Write the failing tests for truncate_marked**

`src/util.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_input_unchanged() {
        assert_eq!(truncate_marked("abc", 5), "abc");
        assert_eq!(truncate_marked("abcde", 5), "abcde");
    }

    #[test]
    fn long_input_truncated_with_marker() {
        assert_eq!(truncate_marked("abcdef", 5), "abcde ...[truncated]");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test util`
Expected: compile error, `truncate_marked` not defined.

- [ ] **Step 3: Implement util and apply the hardening edits**

`src/util.rs`:

```rust
pub fn truncate_marked(s: &str, max_chars: usize) -> String {
    let mut out: String = s.chars().take(max_chars).collect();
    if s.chars().count() > max_chars {
        out.push_str(" ...[truncated]");
    }
    out
}
```

Then apply each edit:

1. `src/main.rs`: add `mod util;`. Replace the tracing init line with:

```rust
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
```

2. `src/main.rs`, in the `SourceCmd::Add` arm, immediately after reading/choosing the secrets text: when the flag value was inline (not `-`), print to stderr:

```rust
            if secrets_json != "-" {
                eprintln!(
                    "warning: inline --secrets-json exposes secrets to the process table and shell history; prefer --secrets-json -"
                );
            }
```

(Place this before parsing so the warning appears even if parsing fails.)

3. `src/store.rs`: change `fn validate_name` to `pub fn validate_name`. In `Store::open`, after the migrations `execute_batch`, add:

```rust
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
```

4. `src/restic.rs`: in `run()`, remove `.arg("-r").arg(&self.repo)` and add `.env("RESTIC_REPOSITORY", &self.repo)` next to the RESTIC_PASSWORD env. Replace the existing inline stderr truncation (the chars().take(2000) block with its marker) with `crate::util::truncate_marked(&String::from_utf8_lossy(&out.stderr), 2000)`.

5. `src/engines/postgres.rs`: replace the stderr `.chars().take(2000).collect::<String>()` expression in the failure bail with `crate::util::truncate_marked(&String::from_utf8_lossy(&out.stderr), 2000)`.

6. `src/pipeline.rs`: first statement of `run_backup` becomes `crate::store::validate_name(&source.name)?;`. Replace the failure-path `let detail: String = format!("{e:#}").chars().take(4000).collect();` with `let detail = crate::util::truncate_marked(&format!("{e:#}"), 4000);`. After `std::fs::create_dir_all(&staging_dir)?;` add unix-only permissions:

```rust
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&staging_dir, std::fs::Permissions::from_mode(0o700))?;
        }
```

7. `Cargo.toml`: delete the `thiserror` and `rand` dependency lines (both unused; chacha20poly1305 re-exports its own OsRng).

8. `.github/workflows/ci.yml`: add `timeout-minutes: 10` under both `test:` and `e2e:` (same indent level as `runs-on`).

9. `tests/e2e_restic.rs`: in the fake pg_dump shim script, make the scan loop bail out when argv is exhausted. The shim content becomes:

```
#!/bin/sh
while [ "$1" != "-f" ]; do
  [ -z "$1" ] && { echo "shim: missing -f" >&2; exit 1; }
  shift
done
echo fakedump > "$2"
```

10. `tests/cli.rs`: in `source_add_then_list_shows_source_without_secrets` (the inline-secrets test), add after the success assert:

```rust
    let add_stderr = String::from_utf8_lossy(&add.stderr);
    assert!(add_stderr.contains("warning: inline --secrets-json"));
```

and in `source_add_reads_secrets_from_stdin`, assert the warning is absent:

```rust
    assert!(!String::from_utf8_lossy(&add.stderr).contains("warning:"));
```

- [ ] **Step 4: Run the full gate**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: util tests pass, both CLI tests pass with the new assertions, all prior tests green.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "fix: apply plan-1 final-review hardening backlog"
```

---

### Task 2: Engine trait returns the backup path; persistent mirror support

**Files:**
- Modify: `src/engines/mod.rs`, `src/engines/postgres.rs`, `src/pipeline.rs`
- Test: updated inline tests in `src/pipeline.rs`

**Interfaces:**
- Consumes: Task 1 (`util::truncate_marked` already in place).
- Produces: `DumpCtx { staging_dir: PathBuf, mirror_root: PathBuf, settings: serde_json::Value, secrets: HashMap<String, String> }`; trait `Engine { fn dump(&self, ctx: &DumpCtx) -> anyhow::Result<PathBuf> }` where the returned path is what restic snapshots; pipeline creates `<staging_root>/.mirrors/<name>` (0700 on unix) before dump and never deletes it.

- [ ] **Step 1: Update the pipeline tests to the new contract (failing first)**

In `src/pipeline.rs` tests: change `OkEngine::dump` to return `Ok(ctx.staging_dir.clone())` after writing the file; change `FailEngine::dump` return type to `Result<PathBuf>`; add a mirror engine and test:

```rust
    struct MirrorEngine;
    impl Engine for MirrorEngine {
        fn dump(&self, ctx: &DumpCtx) -> Result<std::path::PathBuf> {
            std::fs::write(ctx.mirror_root.join("obj1"), b"filedata")?;
            Ok(ctx.mirror_root.clone())
        }
    }

    #[test]
    fn mirror_engine_backs_up_mirror_and_it_survives() {
        let (st, src, staging) = setup();
        let repo = MockRepo::default();
        let out = run_backup(&st, &repo, &src, staging.path(), &MirrorEngine).unwrap();
        assert_eq!(out.status, "success");
        let mirror = staging.path().join(".mirrors").join("acme-db");
        assert!(mirror.join("obj1").exists(), "mirror persists after the run");
        assert!(!staging.path().join("acme-db").exists(), "staging still cleaned");
    }
```

Also update `MockRepo::backup` to record the backed-up path so the mirror test can assert it if needed: `self.calls.borrow_mut().push(format!("backup:{tag}:{}", _path.file_name().and_then(|n| n.to_str()).unwrap_or("?")));` and adjust the success test's expectation to `vec!["backup:source=acme-db:acme-db", "forget:source=acme-db"]` (postgres-style engines return the staging dir, whose final component is the source name).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test pipeline`
Expected: compile errors (Engine::dump returns `()`, no `mirror_root` field).

- [ ] **Step 3: Implement**

`src/engines/mod.rs`:

```rust
pub struct DumpCtx {
    pub staging_dir: PathBuf,
    pub mirror_root: PathBuf,
    pub settings: serde_json::Value,
    pub secrets: HashMap<String, String>,
}

pub trait Engine {
    /// Produce the backup payload; return the directory restic should snapshot.
    fn dump(&self, ctx: &DumpCtx) -> Result<PathBuf>;
}
```

`src/engines/postgres.rs`: `dump` ends with `Ok(ctx.staging_dir.clone())` (signature updated).

`src/pipeline.rs`, inside the closure after creating `staging_dir`:

```rust
        let mirror_root = staging_root.join(".mirrors").join(&source.name);
        std::fs::create_dir_all(&mirror_root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&mirror_root, std::fs::Permissions::from_mode(0o700))?;
        }
        let ctx = DumpCtx {
            staging_dir: staging_dir.clone(),
            mirror_root,
            settings: source.settings.clone(),
            secrets: source.secrets.clone(),
        };
        let backup_path = engine.dump(&ctx)?;
        let tag = format!("source={}", source.name);
        let summary = repo.backup(&backup_path, &tag)?;
```

(Everything else in the closure and the cleanup stays as is; only `staging_dir` is removed afterward.)

- [ ] **Step 4: Run to verify pass**

Run: `cargo test`
Expected: all tests pass including the new mirror test.

- [ ] **Step 5: Commit**

```bash
git add src/engines src/pipeline.rs
git commit -m "feat: engines return backup path, persistent mirror support"
```

---

### Task 3: MongoDB engine

**Files:**
- Create: `src/engines/mongodb.rs`
- Modify: `src/engines/mod.rs` (declare module only; registry arm lands in Task 6)
- Test: inline `#[cfg(test)]` in `src/engines/mongodb.rs`

**Interfaces:**
- Consumes: `DumpCtx`, `Engine` from Task 2; `util::truncate_marked`.
- Produces: `MongodbEngine`; `mongodump_invocation(settings: &serde_json::Value, secrets: &HashMap<String, String>, staging_dir: &Path) -> anyhow::Result<MongoInvocation>` with `pub struct MongoInvocation { pub argv: Vec<String>, pub config_path: PathBuf, pub config_contents: String }`. Settings: optional `db` (string). Secrets: required `uri` (full mongodb:// or mongodb+srv:// connection string). The uri lives ONLY in the config file contents (`uri: <value>`), passed to mongodump via `--config`; output goes to `<staging_dir>/dump`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::Path;

    fn secrets() -> HashMap<String, String> {
        HashMap::from([(
            "uri".to_string(),
            "mongodb://user:pw@db.example.com:27017/app".to_string(),
        )])
    }

    #[test]
    fn builds_argv_with_config_file_and_out_dir() {
        let inv = mongodump_invocation(&serde_json::json!({}), &secrets(), Path::new("/staging/m")).unwrap();
        assert_eq!(
            inv.argv,
            vec![
                "--config",
                "/staging/m/.mongodump-config.yml",
                "--out",
                "/staging/m/dump"
            ]
        );
        assert_eq!(inv.config_contents, "uri: mongodb://user:pw@db.example.com:27017/app\n");
        assert_eq!(inv.config_path, Path::new("/staging/m/.mongodump-config.yml"));
    }

    #[test]
    fn db_setting_appends_db_flag() {
        let inv = mongodump_invocation(&serde_json::json!({"db": "app"}), &secrets(), Path::new("/s")).unwrap();
        assert!(inv.argv.windows(2).any(|w| w == ["--db", "app"]));
    }

    #[test]
    fn uri_never_in_argv() {
        let inv = mongodump_invocation(&serde_json::json!({}), &secrets(), Path::new("/s")).unwrap();
        assert!(!inv.argv.iter().any(|a| a.contains("mongodb://")));
    }

    #[test]
    fn missing_uri_names_the_key() {
        let err = mongodump_invocation(&serde_json::json!({}), &HashMap::new(), Path::new("/s")).unwrap_err();
        assert!(err.to_string().contains("uri"));
    }
}
```

Note for Windows dev machines: the argv assertions compare `Path::join` output; build expected values with the same join calls if literal `/` comparisons fail, e.g. `Path::new("/staging/m").join(".mongodump-config.yml").display().to_string()`. Keep assertions equivalent.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test mongodb`
Expected: compile error, module missing.

- [ ] **Step 3: Implement**

`src/engines/mongodb.rs`:

```rust
use super::{DumpCtx, Engine};
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct MongodbEngine;

pub struct MongoInvocation {
    pub argv: Vec<String>,
    pub config_path: PathBuf,
    pub config_contents: String,
}

pub fn mongodump_invocation(
    settings: &serde_json::Value,
    secrets: &HashMap<String, String>,
    staging_dir: &Path,
) -> Result<MongoInvocation> {
    let uri = secrets
        .get("uri")
        .context("mongodb secrets missing 'uri' (full connection string)")?;
    let config_path = staging_dir.join(".mongodump-config.yml");
    let out_dir = staging_dir.join("dump");
    let mut argv = vec![
        "--config".to_string(),
        config_path.display().to_string(),
        "--out".to_string(),
        out_dir.display().to_string(),
    ];
    if let Some(db) = settings.get("db").and_then(|v| v.as_str()) {
        argv.push("--db".to_string());
        argv.push(db.to_string());
    }
    Ok(MongoInvocation {
        argv,
        config_path,
        config_contents: format!("uri: {uri}\n"),
    })
}

impl Engine for MongodbEngine {
    fn dump(&self, ctx: &DumpCtx) -> Result<PathBuf> {
        let inv = mongodump_invocation(&ctx.settings, &ctx.secrets, &ctx.staging_dir)?;
        {
            use std::io::Write;
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut f = opts
                .open(&inv.config_path)
                .context("failed to create mongodump config file")?;
            f.write_all(inv.config_contents.as_bytes())?;
        }
        let out = Command::new("mongodump")
            .args(&inv.argv)
            .env_remove("VAULTKEEPER_MASTER_KEY")
            .env_remove("RESTIC_PASSWORD")
            .output();
        let _ = std::fs::remove_file(&inv.config_path);
        let out = out.context("failed to spawn mongodump (is mongodb-database-tools installed?)")?;
        if !out.status.success() {
            bail!(
                "mongodump failed: {}",
                crate::util::truncate_marked(&String::from_utf8_lossy(&out.stderr), 2000)
            );
        }
        Ok(ctx.staging_dir.clone())
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test mongodb` then the full gate.
Expected: 4 new tests pass, everything green.

- [ ] **Step 5: Commit**

```bash
git add src/engines
git commit -m "feat: mongodb engine with config-file secret passing"
```

---

### Task 4: Supabase Storage engine

**Files:**
- Create: `src/engines/supabase_storage.rs`
- Modify: `src/engines/mod.rs` (declare module only)
- Test: inline `#[cfg(test)]` in `src/engines/supabase_storage.rs`

**Interfaces:**
- Consumes: `DumpCtx`, `Engine`, `util::truncate_marked`.
- Produces: `SupabaseStorageEngine`; `rclone_invocation(settings: &serde_json::Value, secrets: &HashMap<String, String>, mirror_root: &Path) -> anyhow::Result<(Vec<String>, Vec<(String, String)>)>` returning (argv, env). Settings: required `endpoint` (the project S3 URL), optional `region`. Secrets: required `access_key` and `secret_key`. All credentials flow through `RCLONE_CONFIG_SUPA_*` env vars; argv is `sync SUPA: <mirror_root>`.

- [ ] **Step 1: Write the failing tests**

```rust
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
        let (argv, env) = rclone_invocation(&settings(), &secrets(), Path::new("/staging/.mirrors/x")).unwrap();
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
        let e1 = rclone_invocation(&serde_json::json!({}), &secrets(), Path::new("/m")).unwrap_err();
        assert!(e1.to_string().contains("endpoint"));
        let e2 = rclone_invocation(&settings(), &HashMap::new(), Path::new("/m")).unwrap_err();
        assert!(e2.to_string().contains("access_key"));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test supabase_storage`
Expected: compile error, module missing.

- [ ] **Step 3: Implement**

`src/engines/supabase_storage.rs`:

```rust
use super::{DumpCtx, Engine};
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct SupabaseStorageEngine;

pub fn rclone_invocation(
    settings: &serde_json::Value,
    secrets: &HashMap<String, String>,
    mirror_root: &Path,
) -> Result<(Vec<String>, Vec<(String, String)>)> {
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
        ("RCLONE_CONFIG_SUPA_PROVIDER".to_string(), "Other".to_string()),
        ("RCLONE_CONFIG_SUPA_ACCESS_KEY_ID".to_string(), access_key.clone()),
        ("RCLONE_CONFIG_SUPA_SECRET_ACCESS_KEY".to_string(), secret_key.clone()),
        ("RCLONE_CONFIG_SUPA_ENDPOINT".to_string(), endpoint.to_string()),
    ];
    if let Some(region) = settings.get("region").and_then(|v| v.as_str()) {
        env.push(("RCLONE_CONFIG_SUPA_REGION".to_string(), region.to_string()));
    }
    Ok((argv, env))
}

impl Engine for SupabaseStorageEngine {
    fn dump(&self, ctx: &DumpCtx) -> Result<PathBuf> {
        let (argv, env) = rclone_invocation(&ctx.settings, &ctx.secrets, &ctx.mirror_root)?;
        let out = Command::new("rclone")
            .args(&argv)
            .envs(env)
            .env_remove("VAULTKEEPER_MASTER_KEY")
            .env_remove("RESTIC_PASSWORD")
            .output()
            .context("failed to spawn rclone (is it installed?)")?;
        if !out.status.success() {
            bail!(
                "rclone sync failed: {}",
                crate::util::truncate_marked(&String::from_utf8_lossy(&out.stderr), 2000)
            );
        }
        Ok(ctx.mirror_root.clone())
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test supabase_storage` then the full gate.

- [ ] **Step 5: Commit**

```bash
git add src/engines
git commit -m "feat: supabase storage engine via rclone env config"
```

---

### Task 5: Supabase Functions engine

**Files:**
- Create: `src/engines/supabase_functions.rs`
- Modify: `src/engines/mod.rs` (declare module only), `Cargo.toml` (add reqwest)
- Test: inline `#[cfg(test)]` in `src/engines/supabase_functions.rs`

**Interfaces:**
- Consumes: `DumpCtx`, `Engine`, `util::truncate_marked`.
- Produces: `SupabaseFunctionsEngine`; pure helpers `functions_download_invocation(project_ref: &str) -> Vec<String>` and `auth_config_url(api_base: &str, project_ref: &str) -> String`. Settings: required `project_ref`, optional `api_base` (default `https://api.supabase.com`). Secrets: required `access_token` (Supabase personal access token). The CLI runs with cwd = staging dir and SUPABASE_ACCESS_TOKEN env; auth config is fetched with reqwest (blocking, 30s timeout, bearer auth) and written to `<staging>/auth-config.json`.

- [ ] **Step 1: Add the dependency**

`Cargo.toml` under `[dependencies]`:

```toml
reqwest = { version = "0.12", default-features = false, features = ["blocking", "rustls-tls"] }
```

- [ ] **Step 2: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_invocation_is_exact() {
        assert_eq!(
            functions_download_invocation("abcdefghij1234567890"),
            vec!["functions", "download", "--use-api", "--project-ref", "abcdefghij1234567890"]
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
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test supabase_functions`
Expected: compile error, module missing.

- [ ] **Step 4: Implement**

`src/engines/supabase_functions.rs`:

```rust
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
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut f = opts
                .open(ctx.staging_dir.join("auth-config.json"))
                .context("failed to create auth config file")?;
            f.write_all(&body)?;
        }
        Ok(ctx.staging_dir.clone())
    }
}
```

- [ ] **Step 5: Run to verify pass, then commit**

Run: `cargo test supabase_functions` then the full gate.

```bash
git add src/engines Cargo.toml Cargo.lock
git commit -m "feat: supabase functions engine with auth config export"
```

---

### Task 6: Registry, check-config tools, README roadmap

**Files:**
- Modify: `src/engines/mod.rs`, `src/main.rs`, `README.md`
- Test: updated inline tests in `src/engines/mod.rs`

**Interfaces:**
- Consumes: all three new engines.
- Produces: `engine_for` resolving `postgres | mongodb | supabase_storage | supabase_functions`; check-config verifying `restic, pg_dump, mongodump, rclone, supabase` on PATH.

- [ ] **Step 1: Extend the registry tests (failing first)**

In `src/engines/mod.rs` tests:

```rust
    #[test]
    fn all_engines_resolve() {
        for kind in ["postgres", "mongodb", "supabase_storage", "supabase_functions"] {
            assert!(engine_for(kind).is_ok(), "{kind} should resolve");
        }
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test engines::tests`
Expected: FAIL, mongodb/supabase kinds unknown.

- [ ] **Step 3: Implement**

`src/engines/mod.rs`:

```rust
pub mod mongodb;
pub mod postgres;
pub mod supabase_functions;
pub mod supabase_storage;

pub fn engine_for(kind: &str) -> Result<Box<dyn Engine>> {
    match kind {
        "postgres" => Ok(Box::new(postgres::PostgresEngine)),
        "mongodb" => Ok(Box::new(mongodb::MongodbEngine)),
        "supabase_storage" => Ok(Box::new(supabase_storage::SupabaseStorageEngine)),
        "supabase_functions" => Ok(Box::new(supabase_functions::SupabaseFunctionsEngine)),
        other => bail!("unknown engine kind: {other}"),
    }
}
```

`src/main.rs` check-config: change the tools array to `["restic", "pg_dump", "mongodump", "rclone", "supabase"]`.

`README.md`: change `- [ ] MongoDB, Supabase Storage, Supabase Edge Functions engines` to `- [x]`.

- [ ] **Step 4: Full gate**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: register all four engines, extend check-config, update roadmap"
```

---

## Self-Review Notes

- Spec coverage for plan 2 scope: mongodb engine (directory dump, no gzip: mongodump directory output is uncompressed by default, satisfying the spec's dedup requirement), supabase_storage persistent mirror + rclone via S3 endpoint, supabase_functions CLI download + Management API auth-config export, all secrets via env or 0600 deleted files. Scheduler/notifications intentionally moved to plan 3 (scope decision recorded in the header).
- Type consistency: `DumpCtx` gains `mirror_root` in Task 2 and every engine references the Task 2 signatures; `MongoInvocation` fields used identically in Task 3 test and impl; `truncate_marked` referenced by Tasks 1, 3, 4, 5 with the same signature.
- Placeholder scan: none; all steps carry complete code or exact commands.
- Known API-adaptation authorizations for implementers (note in dispatch): reqwest blocking + rustls feature names, and `Connection::busy_timeout` availability in rusqlite; adapt minimally if the pinned versions differ and disclose in the report.
