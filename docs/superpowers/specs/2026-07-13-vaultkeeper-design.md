# Vaultkeeper: Rust Backup Orchestrator - Design

Date: 2026-07-13
Status: Approved pending user review of this document

## Overview

Vaultkeeper is a self-hosted backup orchestrator: a single Rust binary shipped in a single Docker container that backs up Supabase projects (Postgres database, Storage files, Edge Functions source, auth configuration), vanilla PostgreSQL, and MongoDB to a BorgBase-hosted restic repository. It runs unattended on a schedule, proves its backups restorable with scheduled verify runs, alerts on failure through healthchecks.io, optional webhook, and Amazon SES email, and is operated through a full-control ratatui terminal UI.

## Goals

- Back up all five source kinds: vanilla Postgres, Supabase Postgres, MongoDB, Supabase Storage, Supabase Edge Functions + auth config.
- Pluggable engine abstraction so new database types (MySQL, Redis, ...) are one new module each.
- Set-and-forget: one long-running container, built-in cron scheduling, `docker compose up -d` and done.
- Restic repository on BorgBase: deduplication, encryption, retention pruning, append-only capability.
- Restore as a first-class feature, plus scheduled verify runs that restore into scratch databases and record results.
- Alerting: healthchecks.io dead-man switch per source, optional webhook, SES email.
- Full-control TUI (ratatui): dashboard, history, run/verify/restore, source and credential management.
- Credentials encrypted at rest in SQLite, managed through the TUI, unlocked by a master key from the environment.

## Non-Goals (v1)

- Web UI (the TUI is the management surface; a web layer can be added later on the same core).
- Physical backups / WAL streaming / PITR (hosted Supabase cannot expose that access; scheduled logical dumps are the model).
- Backing up Supabase Edge Function secrets (write-only by design; the team vault is the source of truth).
- Backing up Supabase project settings such as custom domains, network restrictions, pooler config (documented manually; small and rarely changing).
- Durable-execution workflow engines (DBOS/Temporal/Restate). Backup runs are short idempotent batch jobs; a run journal, per-source locking, and alerting cover the need.
- Multi-node coordination. One container owns the schedule.

## Decisions Log

| Decision | Choice | Why |
|---|---|---|
| Scale | Multiple sources, growing to more DB types | Consulting workload; engine trait from day one |
| Deployment | Single long-running container, scheduler inside | No external cron/k8s dependency |
| Repository | BorgBase via restic | Dedupe, encryption, retention, append-only; restic binary is static and also speaks S3/R2 if we ever migrate |
| Restore scope | Restore command + scheduled verify runs | Continuous proof backups are usable |
| Alerting | healthchecks.io + optional webhook + SES email | Dead-man switch plus human-readable failure detail |
| Architecture | Single binary, trait-based engines, shell out to native tools | Simplest structure meeting all requirements |
| UI | ratatui terminal UI, full control including restore | User preference; no network attack surface |
| Credentials | Encrypted at rest in SQLite, TUI-managed | "Add a client in 30 seconds" UX; master key stays in env |

## Architecture

```
vaultkeeper/
├── src/
│   ├── main.rs               # clap subcommands: daemon | run | restore | verify | snapshots | tui | check-config
│   ├── config.rs             # config.toml (global infra) -> typed config, ${ENV} interpolation
│   ├── scheduler.rs          # tokio loop firing cron expressions per source
│   ├── store.rs              # SQLite: sources, credentials (encrypted), runs journal
│   ├── crypto.rs             # ChaCha20-Poly1305 encrypt/decrypt of credential blobs, key from VAULTKEEPER_MASTER_KEY
│   ├── restic.rs             # wrapper: backup, forget/prune, snapshots, restore
│   ├── notify.rs             # healthchecks pings, webhook, SES (aws-sdk-sesv2)
│   ├── engines/
│   │   ├── mod.rs            # trait Source { dump, restore, verify } + registry enum
│   │   ├── postgres.rs       # pg_dump -Fc --compress=0 / pg_restore; optional pg_dumpall globals (vanilla only)
│   │   ├── mongodb.rs        # mongodump directory format (no gzip) / mongorestore
│   │   ├── supabase_storage.rs   # rclone sync from S3-compatible endpoint into persistent mirror
│   │   └── supabase_functions.rs # supabase CLI functions download --use-api; auth config via Management API (reqwest)
│   └── tui/                  # ratatui app: dashboard, history, sources, snapshots, actions
├── Dockerfile                # multi-stage: rust builder -> debian-slim + restic, rclone, postgresql-client-18, mongodb-database-tools, supabase CLI
├── docker-compose.yml        # vaultkeeper service; scratch postgres:18 + mongo services under "verify" profile
└── docs/superpowers/specs/   # this document
```

The binary embeds everything except the native dump tools. reqwest replaces curl for healthchecks pings and Management API calls, so curl is not in the image.

## Configuration Split

- `config.toml` (mounted read-only): infrastructure only. Restic repo URL, staging paths, notification channels (healthchecks base URL, webhook URL, SES region/from/to). Secrets referenced as `${ENV_VAR}`.
- SQLite (`/data/vaultkeeper.db`): sources and their credentials (encrypted), plus the runs journal. Managed through the TUI.
- Environment: `RESTIC_PASSWORD`, `VAULTKEEPER_MASTER_KEY`, SES credentials, and any values referenced from config.toml. Supplied via compose `env_file`.

### config.toml example

```toml
[global]
staging_dir = "/staging"
restic_repo = "sftp:acct@acct.repo.borgbase.com:vaultkeeper"
restic_password = "${RESTIC_PASSWORD}"

[notify]
healthchecks_base = "https://hc-ping.com"
webhook_url = "${SLACK_WEBHOOK}"          # optional
[notify.ses]
region = "us-east-1"
from = "backups@tradelineconsulting.com"
to = ["sal@tradelineconsulting.com"]
```

### SQLite schema (initial)

```sql
CREATE TABLE sources (
  id INTEGER PRIMARY KEY,
  name TEXT UNIQUE NOT NULL,          -- e.g. "acme-supabase-db"
  engine TEXT NOT NULL,               -- postgres | mongodb | supabase_storage | supabase_functions
  schedule TEXT NOT NULL,             -- cron expression
  verify_schedule TEXT,               -- optional cron expression
  retention_json TEXT NOT NULL,       -- {"daily":7,"weekly":4,"monthly":6}
  healthchecks_uuid TEXT,
  settings_json TEXT NOT NULL,        -- non-secret engine settings (host, port, db name, endpoint URL)
  secret_blob BLOB,                   -- ChaCha20-Poly1305 sealed JSON (passwords, keys, tokens)
  enabled INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE runs (
  id INTEGER PRIMARY KEY,
  source_id INTEGER NOT NULL REFERENCES sources(id),
  kind TEXT NOT NULL,                 -- backup | verify | restore | prune
  started_at TEXT NOT NULL,
  finished_at TEXT,
  status TEXT NOT NULL,               -- running | success | failed
  bytes INTEGER,
  snapshot_id TEXT,
  detail TEXT                          -- log tail on failure, row counts on verify
);
```

### Credential encryption

- Key: 32 bytes derived from `VAULTKEEPER_MASTER_KEY` (env) via HKDF-SHA256.
- Cipher: ChaCha20-Poly1305, random 12-byte nonce per blob, nonce stored alongside ciphertext.
- Secrets are decrypted only at the moment a child process needs them and passed via environment variables to that child (PGPASSWORD, RCLONE_CONFIG_* vars, SUPABASE_ACCESS_TOKEN), never written to disk or command lines.
- Losing the master key means re-entering credentials; backups themselves stay restorable because restic has its own password.

## Backup Pipeline

Per source, when its cron fires (or "run now"):

1. Acquire per-source lock (in-process mutex; SQLite `running` status check guards against concurrent `docker exec` invocations).
2. Ping healthchecks `/start`.
3. `engine.dump()` into staging: fresh temp dir for DB dumps (wiped after upload); persistent mirror dir for supabase_storage so rclone transfers deltas only.
4. `restic backup <staging>/<source> --tag source=<name>`.
5. `restic forget --tag source=<name> --keep-daily N --keep-weekly N --keep-monthly N`; `--prune` piggybacks on a weekly schedule rather than every run.
6. Write journal row (status, bytes, snapshot id).
7. Ping healthchecks success, or `/fail` with log tail; on failure also webhook + SES email.

Sources are isolated: a failure in one never blocks others. Transient failures (network, connection reset) get one retry with backoff before being declared failed.

Dumps are written uncompressed (pg_dump `--compress=0`, mongodump without `--gzip`) so restic's chunker sees the redundancy between consecutive nightly dumps; restic compresses on the wire.

### Engine specifics

- **postgres** (covers vanilla and Supabase): `pg_dump -Fc --compress=0`. For vanilla servers, optional `pg_dumpall --globals-only` for roles. For Supabase, globals are skipped (managed by the platform) and the connection guidance is the session pooler (`aws-<region>.pooler.supabase.com:5432`) for IPv4 hosts or direct connection where IPv6 is available.
- **mongodb**: `mongodump --out <dir>` directory format. Restore with `mongorestore --drop`.
- **supabase_storage**: `rclone sync` from the project's S3-compatible endpoint (`https://<ref>.storage.supabase.co/storage/v1/s3`, S3 access keys) into the persistent mirror; restic snapshots the mirror. Restore = rclone sync back.
- **supabase_functions**: `supabase functions download --use-api --project-ref <ref>` for all functions, plus `GET /v1/projects/<ref>/config/auth` (Management API, personal access token) saved as `auth-config.json`. Restore = documented `supabase functions deploy` steps; auth config is a reference JSON for manual re-entry (the PATCH endpoint exists but auto-applying auth config is out of scope for v1).

## Restore and Verify

- `vaultkeeper restore --source <name> --target <url> [--snapshot <id>]`: pulls the snapshot to a temp dir, then `pg_restore --clean --if-exists` or `mongorestore --drop` into the target. Refuses a target whose host matches the source's own host unless `--force-same-host`. TUI restores use the same code path and require typing the source name to confirm.
- `vaultkeeper verify [--source <name>]`: restores the latest snapshot into scratch services (`postgres:18`, `mongo` under the compose `verify` profile), asserts the restore exits cleanly and tables/collections are non-empty, records row counts in the journal detail, and sends a verify report notification. Scheduled per source via `verify_schedule`. If any source has a `verify_schedule`, the stack must be started with `--profile verify` so the scratch services exist; when they are unreachable, the verify run is recorded as failed with a clear "scratch database unreachable" detail and alerts fire, so a misconfigured profile cannot silently skip verification.
- supabase_storage verify: compare mirror file count/bytes against snapshot stats. supabase_functions verify: snapshot contains at least the function directories and auth-config.json.

## TUI (ratatui, full control)

Screens: Dashboard (sources, last/next run, status colors), History (runs, filterable, log tail on failures), Sources (add/edit/disable; credential forms write the encrypted blob), Snapshots (per source, sizes, dates), Actions (run now, verify now, restore with typed-name confirmation).

The TUI runs via `docker exec -it vaultkeeper vaultkeeper tui`. It reads the same SQLite store and invokes the same internal command paths as the CLI. It never holds decrypted secrets in screen state; credential fields display as masked once saved.

## Notifications

- healthchecks.io: per-source check UUID; `/start`, success, `/fail` pings. Catches both failures and the-container-died silence.
- Webhook (optional): JSON POST on failure and on verify completion.
- SES email (aws-sdk-sesv2): on failure and verify reports, using the user's existing SES credentials.
- Layer four is BorgBase's own repo-inactivity monitoring, independent of this tool.

## Docker

- Multi-stage build: `rust:1-bookworm` builder; `debian:bookworm-slim` runtime.
- Runtime packages: `restic`, `rclone`, `postgresql-client-18` (PGDG apt repo; client dumps servers 12 through 18), `mongodb-database-tools`, supabase CLI binary (GitHub release tarball).
- Compose: `vaultkeeper` service (volumes: `/staging`, `/data`; `env_file: .env`; restart unless-stopped) plus `verify-postgres` and `verify-mongo` services under the `verify` profile.

## Error Handling

- `tracing` structured logs to stdout (container logs); optional JSON format.
- Every child process invocation captures stderr; last 50 lines land in the journal `detail` and failure notifications.
- `run` and `restore` exit non-zero on failure for scriptability.
- Scheduler survives individual run panics (per-run task isolation via `tokio::spawn` + join error capture).

## Testing

- Unit: config parsing and env interpolation, retention-to-restic-args mapping, crypto roundtrip, journal state transitions, cron schedule parsing.
- Integration (testcontainers): Postgres and Mongo full roundtrip - seed data, dump, restic backup to a local repo, restore into a second container, assert data equality. Supabase engines tested against recorded HTTP fixtures (Management API) and a MinIO container standing in for the Storage S3 endpoint.
- Smoke: `check-config` validates config.toml, env presence, tool binaries on PATH, restic repo reachability; runs at container start.

## v1 Scope Summary

Ships: four engines, daemon scheduler, restic/BorgBase pipeline, retention, restore command, scheduled verify, healthchecks + webhook + SES notifications, full-control TUI, encrypted credential store, Docker image + compose, tests above.

Later candidates: MySQL/Redis engines, web UI on the same core, auto-apply of Supabase auth config on restore, restic repo health checks (`restic check`) on schedule, multi-repo destinations.
