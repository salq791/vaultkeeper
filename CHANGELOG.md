# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [0.1.0] - 2026-07-14

Initial release: a self-hosted backup orchestrator for Supabase, PostgreSQL, and MongoDB, shipped as a single Rust binary in a single Docker container.

### Added

- Backup engines for Postgres (vanilla and Supabase, via pg_dump), MongoDB (via mongodump), Supabase Storage (via the S3-compatible endpoint), and Supabase Edge Functions source plus auth configuration (via the Management API), each schedulable independently by cron expression.
- A restic-backed repository (BorgBase or any restic backend) for storage: deduplication, encryption, and retention pruning (daily/weekly/monthly).
- A built-in scheduler daemon (`vaultkeeper daemon`): one long-running container, no external cron dependency, with a per-source concurrency guard and stale-run reconciliation at boot.
- A restore command with same-host guards on database restores, snapshot selection, and an explicit overwrite confirmation required for storage restores.
- Scheduled restore verification: restores the latest snapshot into scratch databases and journals the result (row/document counts for database sources, function and auth-config presence for Edge Functions).
- Alerting via a healthchecks.io dead-man switch per source, an optional webhook, and Amazon SES email, with a separate healthchecks UUID available for verify runs.
- Hard timeouts on every child process (dump, restore, verify tools) and graceful shutdown on SIGTERM, so `docker stop` lets in-flight runs finish.
- Encrypted credential storage (ChaCha20-Poly1305, master key supplied via environment variable); secrets are entered via stdin on the CLI or a masked field in the terminal UI, never as a plain command-line argument.
- A terminal UI (ratatui) covering the full operational loop: a dashboard of sources and their last/next run, run history with detail, source management (add/edit/enable/disable) with masked credential entry, snapshot browsing, and a guided restore flow with typed confirmation.
- A `check-config` command that validates configuration, the sources database, required external tools (restic, pg_dump, mongodump, rclone, supabase CLI), and notification/verify wiring, exiting nonzero on any problem.
- A single Docker image (`ghcr.io/salq791/vaultkeeper`, linux/amd64) bundling all required tools, published via CI, with a `docker-compose.yml` that includes an opt-in `verify` profile for the scratch databases used by scheduled verification.
- The daemon now idles instead of exiting when no sources are configured, so a fresh container stays up until you add the first source.

[0.1.0]: https://github.com/salq791/vaultkeeper/releases/tag/v0.1.0
