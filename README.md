# vaultkeeper

Self-hosted backup orchestration for Supabase, PostgreSQL, and MongoDB.
Vaultkeeper runs as one Rust binary in one container and writes scheduled
logical backups to an encrypted, deduplicated restic repository.

> Status: pre-v1. Restore into a disposable target and review the security
> model before relying on it for production recovery.

## What it backs up

- PostgreSQL databases, including Supabase Postgres, with `pg_dump`
- MongoDB with `mongodump`
- Supabase Storage through its S3-compatible endpoint
- Supabase Edge Function source from the Management API, locally mounted
  `deno.json`/import-map files, and the project's Auth configuration

Edge Function secrets, hosted Supabase PITR/WAL, and project-level settings
such as custom domains and network restrictions are outside its scope.

## Safety and operations

- Credentials are encrypted in SQLite with ChaCha20-Poly1305. The master key
  comes from the environment; secrets are accepted through stdin and are not
  placed in child-process arguments.
- PostgreSQL restores use one transaction and fail on the first error.
  MongoDB restores use `--drop --stopOnError`; replica-set backups can capture
  and replay the oplog.
- Every database restore requires the exact source name to be typed. Restores
  to a source endpoint are additionally refused unless explicitly overridden.
- Each backup applies daily/weekly/monthly `restic forget` retention grouped by
  source tag. A separate scheduled maintenance job runs `restic prune` and
  then `restic check`; failures reach configured webhook/SES channels.
- Restore verification replays PostgreSQL and MongoDB snapshots into scratch
  databases. Storage and Edge Function verification is structural: it restores
  the snapshot locally and checks expected files and metadata.
- Run leases are heartbeat-based, so a legitimate long backup is not mistaken
  for an abandoned job. Compose's 14-hour-10-minute shutdown grace covers the
  longest sequence of default child deadlines; increase it if you increase
  those deadlines. The TUI will not quit while its worker is active.

## Deploy

The published image is `ghcr.io/salq791/vaultkeeper:latest` for linux/amd64.
For a new repository:

1. Copy `config.example.toml` to `config.toml` and `.env.example` to `.env`.
   Set `RESTIC_PASSWORD`, a 32-byte hex `VAULTKEEPER_MASTER_KEY` generated with
   `openssl rand -hex 32`, and your restic repository URL.
2. Initialize the restic repository exactly once:

       docker compose run --rm vaultkeeper init-repository

   Do not run this against an existing repository. Vaultkeeper never attempts
   initialization automatically when a repository probe fails.
3. Start the service:

       docker compose up -d

4. Add sources. Pipe secret JSON through stdin so it does not enter shell
   history or the process list. For PostgreSQL:

       echo '{"password":"..."}' | docker compose exec -T vaultkeeper \
         vaultkeeper source add --name my-db --engine postgres \
         --schedule "0 2 * * *" --verify-schedule "0 5 * * 0" \
         --settings-json '{"host":"db.example.com","port":5432,"dbname":"app","user":"postgres"}' \
         --secrets-json -

5. Source changes are loaded when the daemon starts, so restart it after CLI
   or TUI source changes:

       docker compose restart vaultkeeper

6. Validate directories, schedules, timezone, repository access, engine
   settings, scratch URLs, and required executables:

       docker compose exec vaultkeeper vaultkeeper check-config

Cron expressions use five fields and are evaluated in `[global].timezone`, an
IANA name such as `America/Toronto`. The default is `UTC`.

### Source settings

| Engine | Settings JSON | Secrets JSON | Important behavior |
| --- | --- | --- | --- |
| `postgres` | `host`, `port`, `dbname`, `user`; optional `sslmode`, `timeout_minutes` | `password` | Custom-format logical dump |
| `mongodb` | `oplog: true` for a full replica-set dump, or explicitly `allow_inconsistent_dump: true`; optional `db`, `timeout_minutes` | `uri` | `oplog` cannot be combined with `db` |
| `supabase_storage` | `endpoint`; optional `region`, `timeout_minutes` | `access_key`, `secret_key` | Mirrors all buckets through S3 |
| `supabase_functions` | `project_ref`, `local_functions_dir`; optional `api_base`, `timeout_minutes` | `access_token` | Mount `local_functions_dir` read-only in Compose |

MongoDB's recommended consistent mode requires a replica set and captures the
whole replica set:

    echo '{"uri":"mongodb://user:pass@mongo1:27017,mongo2:27017/?replicaSet=rs0"}' |
      docker compose exec -T vaultkeeper vaultkeeper source add \
        --name mongo --engine mongodb --schedule "30 2 * * *" \
        --settings-json '{"oplog":true}' --secrets-json -

The Supabase CLI download endpoint does not include import maps or Deno config.
For Edge Functions, uncomment the read-only functions mount in
`docker-compose.yml` and set `local_functions_dir` to the container path. Only
`deno.json`, `deno.jsonc`, `import_map.json`, and `import_map.jsonc` are copied
from that mount; function code still comes from Supabase.

### Verification

Set `VERIFY_PG_PASSWORD`, start the scratch databases, and configure a source's
`--verify-schedule`:

    docker compose --profile verify up -d
    docker compose exec vaultkeeper vaultkeeper verify --source my-db

Scratch targets must be disposable. Verification cleans/replaces their
contents. It intentionally does not restore Storage back to the live service or
redeploy Edge Functions.

### Restore

List snapshots with:

    docker compose exec vaultkeeper vaultkeeper snapshots --source my-db

Database restore credentials belong in `VAULTKEEPER_RESTORE_TARGET`, not
`--target`. The exact source-name confirmation is always required:

    docker compose exec \
      -e VAULTKEEPER_RESTORE_TARGET='postgres://user:password@recovery-db:5432/app' \
      vaultkeeper vaultkeeper restore --source my-db --confirm-source my-db

If the target shares the source host and port, add `--force-same-host` only
after verifying the destination database. MongoDB uses a MongoDB URI in the
same environment variable.

A Storage restore is destructive and requires `--confirm-remote-overwrite`.
An Edge Functions restore does not deploy anything; it copies the selected
snapshot to `/data/restores/<source>/<snapshot>` and prints manual deployment
steps. The output directory must not already exist, preventing accidental
overwrite.

## Terminal UI

Run:

    docker compose exec -it vaultkeeper vaultkeeper tui

| Key | Action |
| --- | --- |
| Tab / Shift+Tab | Switch tabs |
| Up / Down | Move selection |
| r | Run backup on the selected source |
| v | Run verify on the selected source |
| a / e / d | Add, edit, or enable/disable a source |
| Enter | Load snapshots for the selected source |
| R | Restore the selected snapshot |
| ? | Toggle help |
| q | Quit when no worker is active |

The secrets field is write-only: editing a source never displays stored
credentials, and leaving it blank keeps the existing value. The TUI does not
offer overrides for same-host or remote-overwrite guards; use the CLI when an
override is intentional.

## Recovering Vaultkeeper itself

The restic repository contains source payloads, not Vaultkeeper's control
plane. Back up these separately:

- `config.toml`
- `.env` or, preferably, the secret-manager records that generate it
- `/data/vaultkeeper.db`
- the exact `VAULTKEEPER_MASTER_KEY` and restic credentials

Without the master key, credentials in `vaultkeeper.db` cannot be decrypted.
Keep an offline copy in a secrets manager, never in Git. A recovery drill is:
restore those files onto a clean host, start with the same image version, run
`check-config`, list snapshots, then restore one into disposable scratch
infrastructure.

See [SECURITY.md](SECURITY.md) for plaintext staging and trust-boundary details.

## License

MIT or Apache-2.0, at your option.
