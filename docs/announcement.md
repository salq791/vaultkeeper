> DRAFT: for review before posting. Nothing below is published automatically.

# vaultkeeper: a self-hosted backup orchestrator for Supabase, Postgres, and MongoDB

vaultkeeper is a single Rust binary, run as a single Docker container, that backs up:

- Postgres databases, vanilla or Supabase-hosted (via pg_dump)
- MongoDB (via mongodump)
- Supabase Storage files, through the project's S3-compatible endpoint
- Supabase Edge Functions source and auth configuration, through the Management API

Backing up a Supabase project with pg_dump alone misses Storage and Edge Functions, so this covers all of it from one scheduler.

Backups land in a restic repository (BorgBase or any restic backend), which gives deduplication, encryption, and retention pruning without extra tooling.

Each source can also have a scheduled verify job: it restores its latest snapshot into a scratch database and journals the result (row or document counts, or function/auth-config presence for Edge Functions), so backups are checked, not just taken.

Restore is a first-class command with guards: a restore to what looks like the source's own host is refused unless overridden, and a storage restore that would overwrite the live bucket needs an explicit confirmation flag. A terminal UI covers the whole loop, dashboard, history, source management, snapshot browsing, restore, with credentials entered masked and stored encrypted.

## What it does not do

- Edge Function secrets: these are write-only in Supabase by design, so vaultkeeper does not back them up. Your team's secrets vault stays the source of truth.
- Point-in-time recovery on hosted Supabase: hosted Supabase does not expose the WAL access PITR needs, so vaultkeeper backs up with scheduled logical dumps instead.
- Project settings such as custom domains, network restrictions, or pooler configuration: these change rarely and are left to be documented manually.

## Getting started

The README's Deploy section is the quickstart: `docker compose up -d`, add a source, and the scheduler takes it from there.

Repository: https://github.com/salq791/vaultkeeper

Issues and pull requests are welcome.
