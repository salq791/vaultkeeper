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

## License

MIT or Apache-2.0, at your option.
