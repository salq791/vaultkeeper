# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [0.1.1] - Unreleased

### Fixed

- Preserve Edge Function restores under `/data/restores` instead of deleting
  the reported output with the temporary restore workspace. Capture locally
  mounted Deno and import-map configuration omitted by the Supabase download
  API.
- Require an exact source-name acknowledgement for every database restore,
  compare normalized host/port endpoints, make PostgreSQL cleanup
  single-transactional, and make MongoDB stop on restore errors.
- Require MongoDB operators to choose consistent full-replica-set oplog dumps
  or explicitly acknowledge potentially inconsistent dumps. Oplog backups are
  replayed during restore and verification.
- Keep MongoDB URI files in private runtime tmpfs rather than the persistent
  staging volume and prevent them from entering restic snapshots.
- Use stable restic host identity and tag-only retention grouping. Run costly
  `prune` plus repository `check` on a separate weekly schedule.
- Replace age-based run blocking with a renewable heartbeat lease, refuse TUI
  exit during active work, and give Compose a configurable long shutdown grace
  period.
- Evaluate schedules in an explicit IANA timezone and deepen `check-config`
  validation of repository access, engine settings, directories, and tools.
- Remove vulnerable transitive `rustls-webpki` and `lru` versions. CI now gates
  dependency advisories, leaked secrets, and fixed critical image
  vulnerabilities; release images carry SBOM and provenance attestations.

### Changed

- Repository initialization is an explicit `vaultkeeper init-repository`
  operation. Connectivity or authentication failures never trigger `restic
  init` automatically.
- CLI source secrets are now stdin-only and database restore targets are
  environment-only; password-bearing values are no longer accepted as
  process arguments.
- The partial-success status for a failed `forget` operation is now
  `success_retention_failed`; existing `success_prune_failed` rows remain
  recognized by notification and TUI code.
- Tag builds verify the Cargo version, publish the exact semantic version image
  tag, and create a GitHub Release.

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
[0.1.1]: https://github.com/salq791/vaultkeeper/compare/v0.1.0...v0.1.1
