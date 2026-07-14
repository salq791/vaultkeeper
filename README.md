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

## Features

- Postgres (vanilla and Supabase), MongoDB, Supabase Storage, and Supabase Edge Functions backups on cron schedules
- Restic repositories (BorgBase or any restic backend): deduplication, encryption, retention pruning
- Restore with same-host guards and snapshot selection; storage restores require explicit overwrite confirmation
- Scheduled restore verification into scratch databases with row counts journaled
- healthchecks.io dead-man switch, webhook, and SES email alerting
- Hard timeouts on every child process; graceful SIGTERM shutdown; per-source concurrency guard
- Credentials encrypted at rest (ChaCha20-Poly1305, master key from env), entered via stdin or the masked TUI form, never argv
- Terminal UI for the whole loop: dashboard, history, sources, snapshots, restores

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

4. Scheduled restore verification needs the scratch databases. Set VERIFY_PG_PASSWORD in .env, then: `docker compose --profile verify up -d`, then add `--verify-schedule "0 5 * * 0"` to your sources.
5. `docker compose exec vaultkeeper vaultkeeper check-config` exits nonzero if anything is misconfigured.

Restores: `docker compose exec vaultkeeper vaultkeeper restore --source my-db` (target via the VAULTKEEPER_RESTORE_TARGET environment variable; same-host restores require --force-same-host).

## Terminal UI

The whole operational loop, dashboard, history, source management, snapshot browsing, and restore, is also available as a full-screen terminal UI, running inside the same container:

    docker compose exec -it vaultkeeper vaultkeeper tui

| Key | Action |
| --- | --- |
| Tab / Shift+Tab | Switch tabs |
| Up / Down | Move selection |
| r | Run backup on the selected source |
| v | Run verify on the selected source |
| a | Add a source (Sources tab) |
| e | Edit the selected source (Sources tab) |
| d | Enable/disable the selected source (Sources tab) |
| Enter | Load snapshots for the selected source (Snapshots tab) |
| R | Restore the selected snapshot (Snapshots tab) |
| ? | Toggle this help |
| q | Quit |

Notes:

- Restores started from the TUI go through the same guards as the CLI: a same-host target is refused, and a storage restore that would overwrite the live bucket is refused. Neither guard has a TUI override; use `vaultkeeper restore --force-same-host` or `--confirm-remote-overwrite` on the CLI for those cases.
- The add/edit source form enters credentials masked (shown as asterisks while typing) and only ever writes them out as an encrypted blob. The secrets field is write-only: editing a source never re-displays its stored credentials, and leaving it blank keeps the existing ones.

## License

MIT or Apache-2.0, at your option.
