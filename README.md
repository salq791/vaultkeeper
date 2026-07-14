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
- [x] Core backup path (Postgres -> restic)
- [x] MongoDB, Supabase Storage, Supabase Edge Functions engines
- [x] Built-in scheduler, healthchecks.io / webhook / SES alerting
- [x] Restore command + scheduled restore verification
- [ ] Terminal UI (ratatui) with encrypted credential management

## License

MIT or Apache-2.0, at your option.
