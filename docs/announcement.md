> DRAFT for the upcoming v0.1.1 release. Nothing here is published
> automatically; edit this in your own voice before posting.

# vaultkeeper: verifiable backups for Supabase, Postgres, and MongoDB

vaultkeeper is a self-hosted Rust backup orchestrator, packaged as one Docker
container. It schedules logical backups for:

- vanilla or Supabase-hosted PostgreSQL;
- MongoDB, including consistent full-replica-set oplog capture;
- Supabase Storage through its S3-compatible endpoint; and
- Supabase Edge Function source, Deno/import-map configuration, and Auth
  configuration.

Backups land in an encrypted, deduplicated restic repository. Retention is
applied per source, while repository pruning and integrity checking run on a
separate maintenance schedule.

Database verification restores the latest snapshot into disposable Postgres or
MongoDB instances and records row or document counts. Storage and Edge
Functions verification restores locally and validates snapshot structure; it
does not write to the live service. The CI smoke test also exercises explicit
Postgres, MongoDB, Storage, and durable Edge Functions restore paths inside the
built image.

Restore guardrails require a typed source name for database restores, reject a
matching source endpoint unless deliberately overridden, perform PostgreSQL
cleanup in one transaction, and stop MongoDB on restore errors. Storage restore
requires a separate destructive-overwrite flag. Edge Function restore produces
a durable local directory and manual deployment instructions rather than
silently modifying a project.

The terminal UI covers source management, history, snapshots, run-now,
verification, and guided restore. Stored credentials are encrypted; runtime
MongoDB credential files live only in a private tmpfs. The security guide also
documents the important boundary that local staging payloads are plaintext
until restic encrypts them.

## Deliberate limits

- Supabase Edge Function secrets are write-only and are not backed up. Keep
  your secrets manager as their source of truth.
- Hosted Supabase does not expose the WAL stream needed for vaultkeeper-managed
  PITR, so these are scheduled logical backups.
- Custom domains, network restrictions, pooler settings, and similar project
  configuration remain an operator responsibility.
- Storage and Edge Function scheduled verification is structural rather than a
  live-service writeback test.

Start with the README's deployment, threat-model, and recovery sections, then
run a restore drill against disposable infrastructure before production use.

Repository: https://github.com/salq791/vaultkeeper

Issues and pull requests are welcome.
