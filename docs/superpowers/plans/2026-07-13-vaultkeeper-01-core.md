# Vaultkeeper Plan 1: Core Backup Path Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A public, CI-green Rust project where `vaultkeeper run --source <name>` dumps a Postgres database and ships it to a restic repository, with sources and encrypted credentials stored in SQLite.

**Architecture:** Single binary, single crate. Engines implement a `Engine` trait and shell out to native tools; restic access goes through a `Repo` trait so the pipeline is unit-testable with mocks; SQLite (rusqlite) holds sources, ChaCha20-Poly1305-sealed credentials, and the runs journal.

**Tech Stack:** Rust stable (edition 2021), clap 4 (derive), serde + toml + serde_json, rusqlite (bundled), chacha20poly1305 + hkdf + sha2 + rand, anyhow + thiserror, tracing + tracing-subscriber, tempfile (dev), GitHub Actions CI.

**Spec:** `docs/superpowers/specs/2026-07-13-vaultkeeper-design.md` (approved). This is plan 1 of 4:
1. Core backup path (this plan)
2. Remaining engines (mongodb, supabase_storage, supabase_functions) + daemon scheduler + notifications
3. Restore command + scheduled verify
4. ratatui TUI + Docker image + compose + launch docs

## Global Constraints

- PUBLIC REPO: no secrets, tokens, real hostnames, or real project refs in ANY committed file. Only environment variable NAMES. `.env` is gitignored; `.env.example` carries names and comments only.
- Never use em dashes in any file, code comment, or doc. Use commas, colons, or hyphens.
- `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` must pass at every commit.
- Secrets are passed to child processes via environment variables, never argv.
- Master key env var: `VAULTKEEPER_MASTER_KEY` (64 hex chars, 32 bytes). Restic password env var: `RESTIC_PASSWORD`.
- Config file path: env `VAULTKEEPER_CONFIG`, default `/config/config.toml`. SQLite path: env `VAULTKEEPER_DB`, default `/data/vaultkeeper.db`.
- Conventional commit messages (`feat:`, `test:`, `chore:`, `docs:`).
- Tests must not require network access or real credentials.

---

### Task 1: Public repo scaffold and CI

**Files:**
- Create: `Cargo.toml`, `src/main.rs`, `.gitignore`, `.env.example`, `README.md`, `SECURITY.md`, `LICENSE-MIT`, `LICENSE-APACHE`, `.github/workflows/ci.yml`, `rustfmt.toml`

**Interfaces:**
- Consumes: nothing (first task)
- Produces: compiling binary crate `vaultkeeper` with clap skeleton; CI that later tasks must keep green

- [ ] **Step 1: Create the crate and manifest**

`Cargo.toml`:

```toml
[package]
name = "vaultkeeper"
version = "0.1.0"
edition = "2021"
license = "MIT OR Apache-2.0"
description = "Self-hosted backup orchestrator for Supabase, PostgreSQL, and MongoDB with restic repositories"
repository = "https://github.com/salq791/vaultkeeper"

[dependencies]
anyhow = "1"
thiserror = "2"
clap = { version = "4", features = ["derive"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
rusqlite = { version = "0.32", features = ["bundled"] }
chacha20poly1305 = "0.10"
hkdf = "0.12"
sha2 = "0.10"
rand = "0.8"
hex = "0.4"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

[dev-dependencies]
tempfile = "3"
```

`src/main.rs`:

```rust
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "vaultkeeper", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validate configuration, database, and required tools
    CheckConfig,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();
    let cli = Cli::parse();
    match cli.command {
        Command::CheckConfig => {
            println!("check-config: not yet implemented");
            Ok(())
        }
    }
}
```

`rustfmt.toml`:

```toml
edition = "2021"
```

- [ ] **Step 2: Create public-repo hygiene files**

`.gitignore`:

```
/target
.env
*.db
/staging
```

`.env.example`:

```
# Master key that encrypts stored credentials. Generate with: openssl rand -hex 32
VAULTKEEPER_MASTER_KEY=

# Password for the restic repository (BorgBase or any restic backend)
RESTIC_PASSWORD=

# Optional overrides
# VAULTKEEPER_CONFIG=/config/config.toml
# VAULTKEEPER_DB=/data/vaultkeeper.db
```

`SECURITY.md`:

```markdown
# Security Policy

Vaultkeeper stores database credentials encrypted at rest (ChaCha20-Poly1305,
key from the VAULTKEEPER_MASTER_KEY environment variable) and passes secrets
to child processes only via environment variables.

## Reporting a vulnerability

Please open a GitHub security advisory (Security tab > Report a vulnerability)
rather than a public issue. You will get a response within a week.
```

`README.md`:

```markdown
# vaultkeeper

Self-hosted backup orchestrator for Supabase, PostgreSQL, and MongoDB.
One Rust binary, one container: scheduled logical backups into a
deduplicated, encrypted restic repository (BorgBase or any restic backend),
with restore and scheduled restore-verification as first-class features.

> Status: under active development, pre-v1. The design spec lives in
> [docs/superpowers/specs/](docs/superpowers/specs/).

## Why

Backing up a Supabase project means more than pg_dump: Storage files and
Edge Functions live outside Postgres. Vaultkeeper backs up all of it:

- Postgres databases (vanilla servers and Supabase, via pg_dump)
- MongoDB (via mongodump)
- Supabase Storage files (via the S3-compatible endpoint)
- Supabase Edge Functions source + auth configuration (via the Management API)

Everything lands in a restic repository: deduplication, encryption,
retention pruning, append-only capability.

## Roadmap to v1

- [x] Design spec
- [ ] Core backup path (Postgres -> restic)
- [ ] MongoDB, Supabase Storage, Supabase Edge Functions engines
- [ ] Built-in scheduler, healthchecks.io / webhook / SES alerting
- [ ] Restore command + scheduled restore verification
- [ ] Terminal UI (ratatui) with encrypted credential management

## License

MIT or Apache-2.0, at your option.
```

- [ ] **Step 3: Fetch canonical license texts**

Run:

```bash
curl -sL https://opensource.org/license/mit -o /dev/null # reference only
```

Write `LICENSE-MIT` with the standard MIT text, copyright line:
`Copyright (c) 2026 Tradeline Consulting`. Fetch Apache text verbatim:

```bash
curl -sL https://www.apache.org/licenses/LICENSE-2.0.txt -o LICENSE-APACHE
```

- [ ] **Step 4: Create CI workflow**

`.github/workflows/ci.yml`:

```yaml
name: CI
on:
  push:
    branches: [master, main]
  pull_request:
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@v2
      - run: cargo fmt --check
      - run: cargo clippy --all-targets -- -D warnings
      - run: cargo test
```

- [ ] **Step 5: Verify build and commit**

Run: `cargo build && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: build succeeds, `running 0 tests`, no clippy warnings.

```bash
git add -A
git commit -m "chore: scaffold public crate, CI, licenses, security policy"
```

---

### Task 2: Config loading with env interpolation

**Files:**
- Create: `src/config.rs`
- Modify: `src/main.rs` (add `mod config;`)
- Test: inline `#[cfg(test)]` in `src/config.rs`

**Interfaces:**
- Consumes: nothing
- Produces:
  - `config::Config { global: Global, notify: Notify }`
  - `Global { staging_dir: PathBuf, restic_repo: String, restic_password: String }`
  - `Notify { healthchecks_base: Option<String>, webhook_url: Option<String>, ses: Option<Ses> }`
  - `Ses { region: String, from: String, to: Vec<String> }`
  - `config::load_from_str(toml_text: &str, lookup: &dyn Fn(&str) -> Option<String>) -> anyhow::Result<Config>`
  - `config::load(path: &Path) -> anyhow::Result<Config>` (uses `std::env::var` as lookup)

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[global]
staging_dir = "/staging"
restic_repo = "sftp:demo@demo.repo.borgbase.com:vk"
restic_password = "${RESTIC_PASSWORD}"

[notify]
healthchecks_base = "https://hc-ping.com"
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
        assert_eq!(cfg.notify.healthchecks_base.as_deref(), Some("https://hc-ping.com"));
        assert!(cfg.notify.ses.is_none());
    }

    #[test]
    fn missing_env_var_names_the_variable() {
        let err = load_from_str(SAMPLE, &|_| None).unwrap_err();
        assert!(err.to_string().contains("RESTIC_PASSWORD"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test config`
Expected: compile error, `load_from_str` not defined.

- [ ] **Step 3: Implement**

```rust
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
pub struct Config {
    pub global: Global,
    #[serde(default)]
    pub notify: Notify,
}

#[derive(Debug, Deserialize)]
pub struct Global {
    pub staging_dir: PathBuf,
    pub restic_repo: String,
    pub restic_password: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct Notify {
    pub healthchecks_base: Option<String>,
    pub webhook_url: Option<String>,
    pub ses: Option<Ses>,
}

#[derive(Debug, Deserialize)]
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test config`
Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs src/main.rs
git commit -m "feat: config loading with env interpolation"
```

---

### Task 3: Credential crypto (MasterKey)

**Files:**
- Create: `src/crypto.rs`
- Modify: `src/main.rs` (add `mod crypto;`)
- Test: inline `#[cfg(test)]` in `src/crypto.rs`

**Interfaces:**
- Consumes: nothing
- Produces:
  - `crypto::MasterKey`
  - `MasterKey::from_hex(hex64: &str) -> anyhow::Result<MasterKey>`
  - `MasterKey::from_env() -> anyhow::Result<MasterKey>` (reads `VAULTKEEPER_MASTER_KEY`)
  - `MasterKey::seal(&self, plaintext: &[u8]) -> Vec<u8>` (12-byte random nonce prepended)
  - `MasterKey::open(&self, blob: &[u8]) -> anyhow::Result<Vec<u8>>`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    const K1: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const K2: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    #[test]
    fn roundtrip() {
        let k = MasterKey::from_hex(K1).unwrap();
        let blob = k.seal(b"hunter2");
        assert_eq!(k.open(&blob).unwrap(), b"hunter2");
    }

    #[test]
    fn tampered_blob_fails() {
        let k = MasterKey::from_hex(K1).unwrap();
        let mut blob = k.seal(b"hunter2");
        let last = blob.len() - 1;
        blob[last] ^= 1;
        assert!(k.open(&blob).is_err());
    }

    #[test]
    fn wrong_key_fails() {
        let blob = MasterKey::from_hex(K1).unwrap().seal(b"x");
        assert!(MasterKey::from_hex(K2).unwrap().open(&blob).is_err());
    }

    #[test]
    fn rejects_short_key() {
        assert!(MasterKey::from_hex("abcd").is_err());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test crypto`
Expected: compile error, `MasterKey` not defined.

- [ ] **Step 3: Implement**

```rust
use anyhow::{anyhow, bail, Context, Result};
use chacha20poly1305::aead::{Aead, KeyInit, OsRng};
use chacha20poly1305::{AeadCore, ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;

const NONCE_LEN: usize = 12;

pub struct MasterKey {
    cipher: ChaCha20Poly1305,
}

impl MasterKey {
    pub fn from_hex(hex64: &str) -> Result<Self> {
        let raw = hex::decode(hex64.trim()).context("VAULTKEEPER_MASTER_KEY is not valid hex")?;
        if raw.len() != 32 {
            bail!("VAULTKEEPER_MASTER_KEY must be 32 bytes (64 hex chars), got {}", raw.len());
        }
        let hk = Hkdf::<Sha256>::new(None, &raw);
        let mut okm = [0u8; 32];
        hk.expand(b"vaultkeeper-credentials-v1", &mut okm)
            .map_err(|_| anyhow!("hkdf expand failed"))?;
        Ok(Self { cipher: ChaCha20Poly1305::new(Key::from_slice(&okm)) })
    }

    pub fn from_env() -> Result<Self> {
        let hex64 = std::env::var("VAULTKEEPER_MASTER_KEY")
            .context("VAULTKEEPER_MASTER_KEY is not set")?;
        Self::from_hex(&hex64)
    }

    pub fn seal(&self, plaintext: &[u8]) -> Vec<u8> {
        let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
        let ct = self.cipher.encrypt(&nonce, plaintext).expect("encryption cannot fail");
        let mut blob = nonce.to_vec();
        blob.extend_from_slice(&ct);
        blob
    }

    pub fn open(&self, blob: &[u8]) -> Result<Vec<u8>> {
        if blob.len() <= NONCE_LEN {
            bail!("credential blob too short");
        }
        let (nonce, ct) = blob.split_at(NONCE_LEN);
        self.cipher
            .decrypt(Nonce::from_slice(nonce), ct)
            .map_err(|_| anyhow!("credential decryption failed (wrong master key or corrupt blob)"))
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test crypto`
Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add src/crypto.rs src/main.rs
git commit -m "feat: master-key sealed credential crypto"
```

---

### Task 4: Store (sources + runs journal in SQLite)

**Files:**
- Create: `src/types.rs`, `src/store.rs`
- Modify: `src/main.rs` (add `mod store; mod types;`)
- Test: inline `#[cfg(test)]` in `src/store.rs`

**Interfaces:**
- Consumes: `crypto::MasterKey`
- Produces:
  - `types::Retention { daily: u32, weekly: u32, monthly: u32 }` (serde)
  - `store::Store::open(path: &str, key: MasterKey) -> anyhow::Result<Store>` (":memory:" works; runs migrations)
  - `store::NewSource { name: String, engine: String, schedule: String, verify_schedule: Option<String>, retention: Retention, healthchecks_uuid: Option<String>, settings: serde_json::Value, secrets: HashMap<String, String> }`
  - `store::SourceRow { id: i64, name, engine, schedule, verify_schedule, retention, healthchecks_uuid, settings, secrets: HashMap<String, String>, enabled: bool }` (secrets decrypted)
  - `Store::add_source(&self, s: &NewSource) -> Result<i64>`
  - `Store::get_source(&self, name: &str) -> Result<SourceRow>`
  - `Store::list_sources(&self) -> Result<Vec<SourceRow>>`
  - `Store::start_run(&self, source_id: i64, kind: &str) -> Result<i64>`
  - `Store::finish_run(&self, run_id: i64, status: &str, bytes: Option<i64>, snapshot_id: Option<&str>, detail: Option<&str>) -> Result<()>`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::MasterKey;
    use crate::types::Retention;
    use std::collections::HashMap;

    const K: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    fn store() -> Store {
        Store::open(":memory:", MasterKey::from_hex(K).unwrap()).unwrap()
    }

    fn sample() -> NewSource {
        NewSource {
            name: "acme-db".into(),
            engine: "postgres".into(),
            schedule: "0 2 * * *".into(),
            verify_schedule: None,
            retention: Retention { daily: 7, weekly: 4, monthly: 6 },
            healthchecks_uuid: None,
            settings: serde_json::json!({"host": "db.example.com", "port": 5432, "dbname": "app", "user": "postgres"}),
            secrets: HashMap::from([("password".to_string(), "pw".to_string())]),
        }
    }

    #[test]
    fn add_get_roundtrip_decrypts_secrets() {
        let st = store();
        st.add_source(&sample()).unwrap();
        let row = st.get_source("acme-db").unwrap();
        assert_eq!(row.engine, "postgres");
        assert_eq!(row.secrets.get("password").unwrap(), "pw");
        assert_eq!(row.retention.daily, 7);
        assert!(row.enabled);
    }

    #[test]
    fn duplicate_name_rejected() {
        let st = store();
        st.add_source(&sample()).unwrap();
        assert!(st.add_source(&sample()).is_err());
    }

    #[test]
    fn run_lifecycle() {
        let st = store();
        let sid = st.add_source(&sample()).unwrap();
        let rid = st.start_run(sid, "backup").unwrap();
        st.finish_run(rid, "success", Some(1024), Some("abc123"), None).unwrap();
        let runs = st.recent_runs(10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "success");
        assert_eq!(runs[0].snapshot_id.as_deref(), Some("abc123"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test store`
Expected: compile error, `Store` not defined.

- [ ] **Step 3: Implement**

`src/types.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Retention {
    pub daily: u32,
    pub weekly: u32,
    pub monthly: u32,
}
```

`src/store.rs` (schema per spec; secrets stored as one sealed JSON blob):

```rust
use crate::crypto::MasterKey;
use crate::types::Retention;
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::collections::HashMap;

pub struct Store {
    conn: Connection,
    key: MasterKey,
}

pub struct NewSource {
    pub name: String,
    pub engine: String,
    pub schedule: String,
    pub verify_schedule: Option<String>,
    pub retention: Retention,
    pub healthchecks_uuid: Option<String>,
    pub settings: serde_json::Value,
    pub secrets: HashMap<String, String>,
}

pub struct SourceRow {
    pub id: i64,
    pub name: String,
    pub engine: String,
    pub schedule: String,
    pub verify_schedule: Option<String>,
    pub retention: Retention,
    pub healthchecks_uuid: Option<String>,
    pub settings: serde_json::Value,
    pub secrets: HashMap<String, String>,
    pub enabled: bool,
}

pub struct RunRow {
    pub id: i64,
    pub source_id: i64,
    pub kind: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: String,
    pub bytes: Option<i64>,
    pub snapshot_id: Option<String>,
    pub detail: Option<String>,
}

const MIGRATIONS: &str = r#"
CREATE TABLE IF NOT EXISTS sources (
  id INTEGER PRIMARY KEY,
  name TEXT UNIQUE NOT NULL,
  engine TEXT NOT NULL,
  schedule TEXT NOT NULL,
  verify_schedule TEXT,
  retention_json TEXT NOT NULL,
  healthchecks_uuid TEXT,
  settings_json TEXT NOT NULL,
  secret_blob BLOB,
  enabled INTEGER NOT NULL DEFAULT 1
);
CREATE TABLE IF NOT EXISTS runs (
  id INTEGER PRIMARY KEY,
  source_id INTEGER NOT NULL REFERENCES sources(id),
  kind TEXT NOT NULL,
  started_at TEXT NOT NULL DEFAULT (datetime('now')),
  finished_at TEXT,
  status TEXT NOT NULL,
  bytes INTEGER,
  snapshot_id TEXT,
  detail TEXT
);
"#;

impl Store {
    pub fn open(path: &str, key: MasterKey) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("cannot open db {path}"))?;
        conn.execute_batch(MIGRATIONS)?;
        Ok(Self { conn, key })
    }

    pub fn add_source(&self, s: &NewSource) -> Result<i64> {
        let blob = self.key.seal(serde_json::to_vec(&s.secrets)?.as_slice());
        self.conn.execute(
            "INSERT INTO sources (name, engine, schedule, verify_schedule, retention_json,
             healthchecks_uuid, settings_json, secret_blob)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                s.name,
                s.engine,
                s.schedule,
                s.verify_schedule,
                serde_json::to_string(&s.retention)?,
                s.healthchecks_uuid,
                serde_json::to_string(&s.settings)?,
                blob
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    fn row_to_source(&self, row: &rusqlite::Row) -> Result<SourceRow> {
        let blob: Option<Vec<u8>> = row.get("secret_blob")?;
        let secrets = match blob {
            Some(b) => serde_json::from_slice(&self.key.open(&b)?)?,
            None => HashMap::new(),
        };
        Ok(SourceRow {
            id: row.get("id")?,
            name: row.get("name")?,
            engine: row.get("engine")?,
            schedule: row.get("schedule")?,
            verify_schedule: row.get("verify_schedule")?,
            retention: serde_json::from_str(&row.get::<_, String>("retention_json")?)?,
            healthchecks_uuid: row.get("healthchecks_uuid")?,
            settings: serde_json::from_str(&row.get::<_, String>("settings_json")?)?,
            secrets,
            enabled: row.get::<_, i64>("enabled")? != 0,
        })
    }

    pub fn get_source(&self, name: &str) -> Result<SourceRow> {
        let mut stmt = self.conn.prepare("SELECT * FROM sources WHERE name = ?1")?;
        let mut rows = stmt.query(params![name])?;
        let row = rows.next()?.with_context(|| format!("no source named {name}"))?;
        self.row_to_source(row)
    }

    pub fn list_sources(&self) -> Result<Vec<SourceRow>> {
        let mut stmt = self.conn.prepare("SELECT * FROM sources ORDER BY name")?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(self.row_to_source(row)?);
        }
        Ok(out)
    }

    pub fn start_run(&self, source_id: i64, kind: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO runs (source_id, kind, status) VALUES (?1, ?2, 'running')",
            params![source_id, kind],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn finish_run(
        &self,
        run_id: i64,
        status: &str,
        bytes: Option<i64>,
        snapshot_id: Option<&str>,
        detail: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE runs SET status = ?2, bytes = ?3, snapshot_id = ?4, detail = ?5,
             finished_at = datetime('now') WHERE id = ?1",
            params![run_id, status, bytes, snapshot_id, detail],
        )?;
        Ok(())
    }

    pub fn recent_runs(&self, limit: i64) -> Result<Vec<RunRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source_id, kind, started_at, finished_at, status, bytes, snapshot_id, detail
             FROM runs ORDER BY id DESC LIMIT ?1",
        )?;
        let mut rows = stmt.query(params![limit])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            out.push(RunRow {
                id: r.get(0)?,
                source_id: r.get(1)?,
                kind: r.get(2)?,
                started_at: r.get(3)?,
                finished_at: r.get(4)?,
                status: r.get(5)?,
                bytes: r.get(6)?,
                snapshot_id: r.get(7)?,
                detail: r.get(8)?,
            });
        }
        Ok(out)
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test store`
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add src/types.rs src/store.rs src/main.rs
git commit -m "feat: sqlite store with encrypted credentials and runs journal"
```

---

### Task 5: Restic wrapper behind a Repo trait

**Files:**
- Create: `src/restic.rs`
- Modify: `src/main.rs` (add `mod restic;`)
- Test: inline `#[cfg(test)]` in `src/restic.rs`

**Interfaces:**
- Consumes: `types::Retention`
- Produces:
  - `restic::Repo` trait:
    - `fn ensure_init(&self) -> anyhow::Result<()>`
    - `fn backup(&self, path: &Path, tag: &str) -> anyhow::Result<BackupSummary>`
    - `fn forget(&self, tag: &str, retention: &Retention) -> anyhow::Result<()>`
    - `fn snapshots(&self, tag: Option<&str>) -> anyhow::Result<Vec<Snapshot>>`
  - `restic::BackupSummary { snapshot_id: String, total_bytes_processed: i64 }`
  - `restic::Snapshot { id: String, time: String, tags: Vec<String> }`
  - `restic::ResticCli::new(repo: String, password: String) -> ResticCli` (implements `Repo` by spawning the `restic` binary with `--json`; password via `RESTIC_PASSWORD` env on the child, never argv)
  - Pure helpers (unit-tested): `parse_backup_output(&str) -> Result<BackupSummary>`, `parse_snapshots(&str) -> Result<Vec<Snapshot>>`, `forget_args(tag: &str, r: &Retention) -> Vec<String>`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Retention;

    #[test]
    fn parses_backup_summary_line() {
        let out = concat!(
            r#"{"message_type":"status","percent_done":1}"#, "\n",
            r#"{"message_type":"summary","snapshot_id":"a1b2c3","total_bytes_processed":52428800}"#, "\n"
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
        let r = Retention { daily: 7, weekly: 4, monthly: 6 };
        assert_eq!(
            forget_args("source=acme-db", &r),
            vec![
                "forget", "--tag", "source=acme-db",
                "--keep-daily", "7", "--keep-weekly", "4", "--keep-monthly", "6", "--json"
            ]
        );
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test restic`
Expected: compile error, functions not defined.

- [ ] **Step 3: Implement**

```rust
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

#[derive(Debug, Deserialize)]
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
        "forget".into(), "--tag".into(), tag.into(),
        "--keep-daily".into(), r.daily.to_string(),
        "--keep-weekly".into(), r.weekly.to_string(),
        "--keep-monthly".into(), r.monthly.to_string(),
        "--json".into(),
    ]
}

pub struct ResticCli {
    repo: String,
    password: String,
    bin: String,
}

impl ResticCli {
    pub fn new(repo: String, password: String) -> Self {
        Self { repo, password, bin: "restic".into() }
    }

    fn run(&self, args: &[String]) -> Result<String> {
        let out = Command::new(&self.bin)
            .arg("-r").arg(&self.repo)
            .args(args)
            .env("RESTIC_PASSWORD", &self.password)
            .output()
            .with_context(|| format!("failed to spawn {}", self.bin))?;
        if !out.status.success() {
            bail!(
                "restic {} failed: {}",
                args.first().map(String::as_str).unwrap_or(""),
                String::from_utf8_lossy(&out.stderr).chars().take(2000).collect::<String>()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

impl Repo for ResticCli {
    fn ensure_init(&self) -> Result<()> {
        let probe = self.run(&["cat".into(), "config".into()]);
        if probe.is_ok() {
            return Ok(());
        }
        self.run(&["init".into()]).map(|_| ())
    }

    fn backup(&self, path: &Path, tag: &str) -> Result<BackupSummary> {
        let out = self.run(&[
            "backup".into(), path.display().to_string(),
            "--tag".into(), tag.into(), "--json".into(),
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
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test restic`
Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add src/restic.rs src/main.rs
git commit -m "feat: restic wrapper behind Repo trait with parsed json output"
```

---

### Task 6: Engine trait and Postgres engine

**Files:**
- Create: `src/engines/mod.rs`, `src/engines/postgres.rs`
- Modify: `src/main.rs` (add `mod engines;`)
- Test: inline `#[cfg(test)]` in `src/engines/postgres.rs` and `src/engines/mod.rs`

**Interfaces:**
- Consumes: `store::SourceRow` fields (`settings: serde_json::Value`, `secrets: HashMap<String, String>`)
- Produces:
  - `engines::DumpCtx { staging_dir: PathBuf, settings: serde_json::Value, secrets: HashMap<String, String> }`
  - `engines::Engine` trait: `fn dump(&self, ctx: &DumpCtx) -> anyhow::Result<()>`
  - `engines::engine_for(kind: &str) -> anyhow::Result<Box<dyn Engine>>` (knows "postgres"; errors otherwise)
  - `engines::postgres::pg_dump_invocation(settings, secrets, out_file) -> anyhow::Result<(Vec<String>, Vec<(String, String)>)>` returning (argv, extra env)

- [ ] **Step 1: Write the failing tests**

In `src/engines/postgres.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::Path;

    fn settings() -> serde_json::Value {
        serde_json::json!({"host": "db.example.com", "port": 5432, "dbname": "app", "user": "postgres", "sslmode": "require"})
    }

    #[test]
    fn builds_pg_dump_argv_and_env() {
        let secrets = HashMap::from([("password".to_string(), "pw".to_string())]);
        let (argv, env) =
            pg_dump_invocation(&settings(), &secrets, Path::new("/staging/x/db.dump")).unwrap();
        assert_eq!(
            argv,
            vec![
                "-h", "db.example.com", "-p", "5432", "-U", "postgres",
                "-Fc", "--compress=0", "-f", "/staging/x/db.dump", "app"
            ]
        );
        assert!(env.contains(&("PGPASSWORD".to_string(), "pw".to_string())));
        assert!(env.contains(&("PGSSLMODE".to_string(), "require".to_string())));
    }

    #[test]
    fn missing_password_is_error() {
        let err = pg_dump_invocation(&settings(), &HashMap::new(), Path::new("/tmp/x")).unwrap_err();
        assert!(err.to_string().contains("password"));
    }
}
```

In `src/engines/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_engine_is_error() {
        assert!(engine_for("clippydb").is_err());
    }

    #[test]
    fn postgres_engine_resolves() {
        assert!(engine_for("postgres").is_ok());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test engines`
Expected: compile error, modules not defined.

- [ ] **Step 3: Implement**

`src/engines/mod.rs`:

```rust
pub mod postgres;

use anyhow::{bail, Result};
use std::collections::HashMap;
use std::path::PathBuf;

pub struct DumpCtx {
    pub staging_dir: PathBuf,
    pub settings: serde_json::Value,
    pub secrets: HashMap<String, String>,
}

pub trait Engine {
    fn dump(&self, ctx: &DumpCtx) -> Result<()>;
}

pub fn engine_for(kind: &str) -> Result<Box<dyn Engine>> {
    match kind {
        "postgres" => Ok(Box::new(postgres::PostgresEngine)),
        other => bail!("unknown engine kind: {other}"),
    }
}
```

`src/engines/postgres.rs`:

```rust
use super::{DumpCtx, Engine};
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

pub struct PostgresEngine;

pub fn pg_dump_invocation(
    settings: &serde_json::Value,
    secrets: &HashMap<String, String>,
    out_file: &Path,
) -> Result<(Vec<String>, Vec<(String, String)>)> {
    let get = |k: &str| -> Result<String> {
        Ok(settings
            .get(k)
            .with_context(|| format!("postgres settings missing '{k}'"))?
            .to_string()
            .trim_matches('"')
            .to_string())
    };
    let password = secrets
        .get("password")
        .context("postgres secrets missing 'password'")?
        .clone();

    let argv = vec![
        "-h".into(), get("host")?,
        "-p".into(), get("port")?,
        "-U".into(), get("user")?,
        "-Fc".into(), "--compress=0".into(),
        "-f".into(), out_file.display().to_string(),
        get("dbname")?,
    ];
    let mut env = vec![("PGPASSWORD".to_string(), password)];
    if let Some(ssl) = settings.get("sslmode").and_then(|v| v.as_str()) {
        env.push(("PGSSLMODE".to_string(), ssl.to_string()));
    }
    Ok((argv, env))
}

impl Engine for PostgresEngine {
    fn dump(&self, ctx: &DumpCtx) -> Result<()> {
        let out_file = ctx.staging_dir.join("db.dump");
        let (argv, env) = pg_dump_invocation(&ctx.settings, &ctx.secrets, &out_file)?;
        let out = Command::new("pg_dump")
            .args(&argv)
            .envs(env)
            .output()
            .context("failed to spawn pg_dump (is it installed and on PATH?)")?;
        if !out.status.success() {
            bail!(
                "pg_dump failed: {}",
                String::from_utf8_lossy(&out.stderr).chars().take(2000).collect::<String>()
            );
        }
        Ok(())
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test engines`
Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add src/engines src/main.rs
git commit -m "feat: engine trait and postgres pg_dump engine"
```

---

### Task 7: Backup pipeline

**Files:**
- Create: `src/pipeline.rs`
- Modify: `src/main.rs` (add `mod pipeline;`)
- Test: inline `#[cfg(test)]` in `src/pipeline.rs`

**Interfaces:**
- Consumes: `Store`, `Repo` trait, `engines::engine_for`, `SourceRow`
- Produces:
  - `pipeline::run_backup(store: &Store, repo: &dyn Repo, source: &SourceRow, staging_root: &Path, engine: &dyn Engine) -> anyhow::Result<RunOutcome>`
  - `pipeline::RunOutcome { run_id: i64, snapshot_id: Option<String>, status: String }`
  - Behavior contract: journal row created before dump and finished afterward; staging subdir `<staging_root>/<source.name>` created fresh and removed afterward in both success and failure paths; tag format `source=<name>`; forget runs after successful backup.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::MasterKey;
    use crate::engines::{DumpCtx, Engine};
    use crate::restic::{BackupSummary, Repo, Snapshot};
    use crate::store::{NewSource, Store};
    use crate::types::Retention;
    use anyhow::{bail, Result};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::path::Path;

    const K: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    struct OkEngine;
    impl Engine for OkEngine {
        fn dump(&self, ctx: &DumpCtx) -> Result<()> {
            std::fs::write(ctx.staging_dir.join("db.dump"), b"data")?;
            Ok(())
        }
    }

    struct FailEngine;
    impl Engine for FailEngine {
        fn dump(&self, _ctx: &DumpCtx) -> Result<()> {
            bail!("connection refused");
        }
    }

    #[derive(Default)]
    struct MockRepo {
        calls: RefCell<Vec<String>>,
    }
    impl Repo for MockRepo {
        fn ensure_init(&self) -> Result<()> {
            Ok(())
        }
        fn backup(&self, _path: &Path, tag: &str) -> Result<BackupSummary> {
            self.calls.borrow_mut().push(format!("backup:{tag}"));
            Ok(BackupSummary { snapshot_id: "snap1".into(), total_bytes_processed: 4 })
        }
        fn forget(&self, tag: &str, _r: &Retention) -> Result<()> {
            self.calls.borrow_mut().push(format!("forget:{tag}"));
            Ok(())
        }
        fn snapshots(&self, _tag: Option<&str>) -> Result<Vec<Snapshot>> {
            Ok(vec![])
        }
    }

    fn setup() -> (Store, crate::store::SourceRow, tempfile::TempDir) {
        let st = Store::open(":memory:", MasterKey::from_hex(K).unwrap()).unwrap();
        st.add_source(&NewSource {
            name: "acme-db".into(),
            engine: "postgres".into(),
            schedule: "0 2 * * *".into(),
            verify_schedule: None,
            retention: Retention { daily: 7, weekly: 4, monthly: 6 },
            healthchecks_uuid: None,
            settings: serde_json::json!({}),
            secrets: HashMap::new(),
        })
        .unwrap();
        let src = st.get_source("acme-db").unwrap();
        (st, src, tempfile::tempdir().unwrap())
    }

    #[test]
    fn success_path_backs_up_forgets_and_journals() {
        let (st, src, staging) = setup();
        let repo = MockRepo::default();
        let out = run_backup(&st, &repo, &src, staging.path(), &OkEngine).unwrap();
        assert_eq!(out.status, "success");
        assert_eq!(out.snapshot_id.as_deref(), Some("snap1"));
        assert_eq!(
            *repo.calls.borrow(),
            vec!["backup:source=acme-db", "forget:source=acme-db"]
        );
        assert!(!staging.path().join("acme-db").exists(), "staging cleaned");
        let runs = st.recent_runs(1).unwrap();
        assert_eq!(runs[0].status, "success");
        assert_eq!(runs[0].bytes, Some(4));
    }

    #[test]
    fn failure_path_journals_detail_and_cleans_staging() {
        let (st, src, staging) = setup();
        let repo = MockRepo::default();
        let err = run_backup(&st, &repo, &src, staging.path(), &FailEngine).unwrap_err();
        assert!(err.to_string().contains("connection refused"));
        assert!(repo.calls.borrow().is_empty(), "no restic calls on dump failure");
        assert!(!staging.path().join("acme-db").exists(), "staging cleaned");
        let runs = st.recent_runs(1).unwrap();
        assert_eq!(runs[0].status, "failed");
        assert!(runs[0].detail.as_deref().unwrap().contains("connection refused"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test pipeline`
Expected: compile error, `run_backup` not defined.

- [ ] **Step 3: Implement**

```rust
use crate::engines::{DumpCtx, Engine};
use crate::restic::Repo;
use crate::store::{SourceRow, Store};
use anyhow::Result;
use std::path::Path;

pub struct RunOutcome {
    pub run_id: i64,
    pub snapshot_id: Option<String>,
    pub status: String,
}

pub fn run_backup(
    store: &Store,
    repo: &dyn Repo,
    source: &SourceRow,
    staging_root: &Path,
    engine: &dyn Engine,
) -> Result<RunOutcome> {
    let run_id = store.start_run(source.id, "backup")?;
    let staging_dir = staging_root.join(&source.name);
    let result = (|| -> Result<(String, i64)> {
        if staging_dir.exists() {
            std::fs::remove_dir_all(&staging_dir)?;
        }
        std::fs::create_dir_all(&staging_dir)?;
        let ctx = DumpCtx {
            staging_dir: staging_dir.clone(),
            settings: source.settings.clone(),
            secrets: source.secrets.clone(),
        };
        engine.dump(&ctx)?;
        let tag = format!("source={}", source.name);
        let summary = repo.backup(&staging_dir, &tag)?;
        repo.forget(&tag, &source.retention)?;
        Ok((summary.snapshot_id, summary.total_bytes_processed))
    })();
    let _ = std::fs::remove_dir_all(&staging_dir);

    match result {
        Ok((snapshot_id, bytes)) => {
            store.finish_run(run_id, "success", Some(bytes), Some(&snapshot_id), None)?;
            Ok(RunOutcome { run_id, snapshot_id: Some(snapshot_id), status: "success".into() })
        }
        Err(e) => {
            let detail: String = format!("{e:#}").chars().take(4000).collect();
            store.finish_run(run_id, "failed", None, None, Some(&detail))?;
            Err(e)
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test pipeline`
Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add src/pipeline.rs src/main.rs
git commit -m "feat: backup pipeline with journaling and staging cleanup"
```

---

### Task 8: CLI wiring (source add/list, run, snapshots, check-config)

**Files:**
- Modify: `src/main.rs` (full rewrite shown below)
- Test: `tests/cli.rs`

**Interfaces:**
- Consumes: everything above
- Produces the user-facing CLI:
  - `vaultkeeper source add --name N --engine postgres --schedule CRON --settings-json J --secrets-json J [--retention D,W,M] [--healthchecks-uuid U]`
  - `vaultkeeper source list`
  - `vaultkeeper run --source N`
  - `vaultkeeper snapshots [--source N]`
  - `vaultkeeper check-config`

- [ ] **Step 1: Write the failing test**

`tests/cli.rs` (integration test drives the real binary with a temp db and config; no network, no real tools needed for these paths):

```rust
use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vaultkeeper"))
}

const K: &str = "1111111111111111111111111111111111111111111111111111111111111111";

#[test]
fn source_add_then_list_shows_source_without_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("vk.db");
    let add = bin()
        .env("VAULTKEEPER_MASTER_KEY", K)
        .env("VAULTKEEPER_DB", &db)
        .args([
            "source", "add",
            "--name", "acme-db",
            "--engine", "postgres",
            "--schedule", "0 2 * * *",
            "--settings-json", r#"{"host":"db.example.com","port":5432,"dbname":"app","user":"postgres"}"#,
            "--secrets-json", r#"{"password":"pw"}"#,
        ])
        .output()
        .unwrap();
    assert!(add.status.success(), "{}", String::from_utf8_lossy(&add.stderr));

    let list = bin()
        .env("VAULTKEEPER_MASTER_KEY", K)
        .env("VAULTKEEPER_DB", &db)
        .args(["source", "list"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(stdout.contains("acme-db"));
    assert!(stdout.contains("postgres"));
    assert!(!stdout.contains("pw"), "secrets must never be printed");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test cli`
Expected: FAIL, `source` subcommand unknown.

- [ ] **Step 3: Implement `src/main.rs`**

```rust
mod config;
mod crypto;
mod engines;
mod pipeline;
mod restic;
mod store;
mod types;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "vaultkeeper", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Manage backup sources
    Source {
        #[command(subcommand)]
        cmd: SourceCmd,
    },
    /// Run a backup now
    Run {
        #[arg(long)]
        source: String,
    },
    /// List snapshots in the repository
    Snapshots {
        #[arg(long)]
        source: Option<String>,
    },
    /// Validate configuration, database, and required tools
    CheckConfig,
}

#[derive(Subcommand)]
enum SourceCmd {
    Add {
        #[arg(long)]
        name: String,
        #[arg(long)]
        engine: String,
        #[arg(long)]
        schedule: String,
        #[arg(long)]
        settings_json: String,
        #[arg(long)]
        secrets_json: String,
        /// daily,weekly,monthly (default 7,4,6)
        #[arg(long, default_value = "7,4,6")]
        retention: String,
        #[arg(long)]
        healthchecks_uuid: Option<String>,
    },
    List,
}

fn db_path() -> String {
    std::env::var("VAULTKEEPER_DB").unwrap_or_else(|_| "/data/vaultkeeper.db".into())
}

fn config_path() -> PathBuf {
    std::env::var("VAULTKEEPER_CONFIG")
        .unwrap_or_else(|_| "/config/config.toml".into())
        .into()
}

fn open_store() -> Result<store::Store> {
    store::Store::open(&db_path(), crypto::MasterKey::from_env()?)
}

fn parse_retention(s: &str) -> Result<types::Retention> {
    let parts: Vec<u32> = s
        .split(',')
        .map(|p| p.trim().parse::<u32>().context("retention must be daily,weekly,monthly numbers"))
        .collect::<Result<_>>()?;
    anyhow::ensure!(parts.len() == 3, "retention must have exactly three numbers: daily,weekly,monthly");
    Ok(types::Retention { daily: parts[0], weekly: parts[1], monthly: parts[2] })
}

fn tool_on_path(name: &str) -> bool {
    which_path(name).is_some()
}

fn which_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for candidate in [dir.join(name), dir.join(format!("{name}.exe"))] {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn main() -> Result<()> {
    tracing_subscriber::fmt().init();
    let cli = Cli::parse();
    match cli.command {
        Command::Source { cmd } => match cmd {
            SourceCmd::Add {
                name, engine, schedule, settings_json, secrets_json, retention, healthchecks_uuid,
            } => {
                engines::engine_for(&engine)?;
                let st = open_store()?;
                st.add_source(&store::NewSource {
                    name: name.clone(),
                    engine,
                    schedule,
                    verify_schedule: None,
                    retention: parse_retention(&retention)?,
                    healthchecks_uuid,
                    settings: serde_json::from_str(&settings_json).context("invalid --settings-json")?,
                    secrets: serde_json::from_str::<HashMap<String, String>>(&secrets_json)
                        .context("invalid --secrets-json")?,
                })?;
                println!("added source {name}");
                Ok(())
            }
            SourceCmd::List => {
                let st = open_store()?;
                for s in st.list_sources()? {
                    println!(
                        "{}\t{}\t{}\t{}",
                        s.name,
                        s.engine,
                        s.schedule,
                        if s.enabled { "enabled" } else { "disabled" }
                    );
                }
                Ok(())
            }
        },
        Command::Run { source } => {
            let cfg = config::load(&config_path())?;
            let st = open_store()?;
            let src = st.get_source(&source)?;
            let engine = engines::engine_for(&src.engine)?;
            let repo = restic::ResticCli::new(cfg.global.restic_repo, cfg.global.restic_password);
            use restic::Repo as _;
            repo.ensure_init()?;
            let out = pipeline::run_backup(&st, &repo, &src, &cfg.global.staging_dir, engine.as_ref())?;
            println!("backup of {source} complete, snapshot {}", out.snapshot_id.unwrap_or_default());
            Ok(())
        }
        Command::Snapshots { source } => {
            let cfg = config::load(&config_path())?;
            let repo = restic::ResticCli::new(cfg.global.restic_repo, cfg.global.restic_password);
            use restic::Repo as _;
            let tag = source.map(|s| format!("source={s}"));
            for snap in repo.snapshots(tag.as_deref())? {
                println!("{}\t{}\t{}", snap.id, snap.time, snap.tags.join(","));
            }
            Ok(())
        }
        Command::CheckConfig => {
            let cfg = config::load(&config_path())?;
            let st = open_store()?;
            let sources = st.list_sources()?;
            println!("config ok: staging={}", cfg.global.staging_dir.display());
            println!("db ok: {} sources", sources.len());
            for tool in ["restic", "pg_dump"] {
                println!(
                    "{tool}: {}",
                    if tool_on_path(tool) { "found" } else { "MISSING from PATH" }
                );
            }
            Ok(())
        }
    }
}
```

- [ ] **Step 4: Run all tests to verify they pass**

Run: `cargo test`
Expected: all unit tests plus `tests/cli.rs` pass.

- [ ] **Step 5: Lint, then commit**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: clean.

```bash
git add -A
git commit -m "feat: cli wiring for source management, run, snapshots, check-config"
```

---

### Task 9: End-to-end smoke against real restic (CI only)

**Files:**
- Create: `tests/e2e_restic.rs`
- Modify: `.github/workflows/ci.yml` (install restic, run ignored tests)

**Interfaces:**
- Consumes: the built binary and a real `restic` on PATH
- Produces: proof the pipeline works with real restic using a local repo and a fake `pg_dump` shim; runs in CI, `#[ignore]`d locally

- [ ] **Step 1: Write the ignored e2e test**

```rust
//! Runs only where restic is installed: `cargo test --test e2e_restic -- --ignored`
use std::process::Command;

const K: &str = "1111111111111111111111111111111111111111111111111111111111111111";

#[test]
#[ignore = "requires restic on PATH; runs in CI"]
fn full_backup_into_local_restic_repo() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    let staging = dir.path().join("staging");
    let db = dir.path().join("vk.db");
    let cfg_path = dir.path().join("config.toml");
    std::fs::write(
        &cfg_path,
        format!(
            "[global]\nstaging_dir = \"{}\"\nrestic_repo = \"{}\"\nrestic_password = \"testpw\"\n",
            staging.display().to_string().replace('\\', "/"),
            repo.display().to_string().replace('\\', "/"),
        ),
    )
    .unwrap();

    // fake pg_dump: a shim directory prepended to PATH that writes a file
    let shim = dir.path().join("shim");
    std::fs::create_dir_all(&shim).unwrap();
    let script = shim.join("pg_dump");
    std::fs::write(&script, "#!/bin/sh\nwhile [ \"$1\" != \"-f\" ]; do shift; done\necho fakedump > \"$2\"\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path_env = format!(
        "{}:{}",
        shim.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let run = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_vaultkeeper"))
            .env("VAULTKEEPER_MASTER_KEY", K)
            .env("VAULTKEEPER_DB", &db)
            .env("VAULTKEEPER_CONFIG", &cfg_path)
            .env("PATH", &path_env)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{:?}: {}", args, String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    run(&[
        "source", "add", "--name", "e2e-db", "--engine", "postgres",
        "--schedule", "0 2 * * *",
        "--settings-json", r#"{"host":"localhost","port":5432,"dbname":"app","user":"postgres"}"#,
        "--secrets-json", r#"{"password":"x"}"#,
    ]);
    let out = run(&["run", "--source", "e2e-db"]);
    assert!(out.contains("snapshot"));
    let snaps = run(&["snapshots", "--source", "e2e-db"]);
    assert!(snaps.contains("source=e2e-db"));
}
```

- [ ] **Step 2: Run locally to confirm it is skipped**

Run: `cargo test --test e2e_restic`
Expected: `1 ignored`.

- [ ] **Step 3: Add CI job**

Append to `.github/workflows/ci.yml` jobs:

```yaml
  e2e:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - run: sudo apt-get update && sudo apt-get install -y restic
      - run: cargo test --test e2e_restic -- --ignored
```

- [ ] **Step 4: Push and verify CI is green**

Run: `git add -A && git commit -m "test: e2e smoke against real restic in CI" && git push`
Expected: both CI jobs pass on GitHub.

- [ ] **Step 5: Update README roadmap checkbox**

Change `- [ ] Core backup path (Postgres -> restic)` to `- [x]` in `README.md`.

```bash
git add README.md
git commit -m "docs: mark core backup path done in roadmap"
git push
```

---

## Self-Review Notes

- Spec coverage for plan 1 scope: config split (Task 2 + Task 4), credential encryption exactly as spec (HKDF-SHA256, ChaCha20-Poly1305, nonce prepended, env-only child secrets: Tasks 3, 6), schema matches spec (Task 4), uncompressed dumps `-Fc --compress=0` (Task 6), tag format `source=<name>` and retention mapping to `--keep-*` (Tasks 5, 7), staging fresh-per-run and cleanup on both paths (Task 7), public-repo hygiene and no-secrets rule (Task 1, Global Constraints). Notifications, scheduler, other engines, restore/verify, TUI: plans 2-4 by design.
- Type consistency: `Retention` defined once in `types.rs`; `Repo` and `Engine` trait signatures used identically in Tasks 5-8; `SourceRow.secrets` is `HashMap<String, String>` throughout.
- Windows note: development happens on Windows but CI and the container are Linux; the e2e test's shell shim is unix-only, which is why it is `#[ignore]`d and executed in CI.
