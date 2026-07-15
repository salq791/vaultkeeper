# Security Policy

## Security model

Vaultkeeper encrypts source credentials stored in `vaultkeeper.db` with
ChaCha20-Poly1305. `VAULTKEEPER_MASTER_KEY` is the encryption root and must be
kept outside Git, backed up independently, and restricted like a production
database credential. Anyone with both the database and master key can decrypt
all stored source credentials.

Secrets are accepted through stdin by the CLI and a masked TUI field. Database
passwords are passed to PostgreSQL tools through environment variables. The
MongoDB URI is written to a mode-0600 temporary config file under
`/run/vaultkeeper`, which Compose mounts as a private tmpfs; it is closed and
removed after the child process. Secrets are never intentionally placed in a
child process's command-line arguments. Host root, the container runtime, and
processes with sufficient same-user inspection rights remain trusted. The
environment-only restore target is removed before Vaultkeeper starts restic or
an engine child; each database tool receives only its parsed target fields.

## Plaintext backup staging

Restic encrypts data in the repository, but source payloads are plaintext
before restic ingests them:

- database and Edge Function exports exist temporarily under `/staging`;
- the Supabase Storage mirror persists under `/staging/.mirrors` so `rclone`
  can synchronize it efficiently;
- manually restored Edge Function files persist under `/data/restores`.

The default Compose configuration uses Docker volumes for `/staging` and
`/data`; Docker volumes are not encryption at rest. Run Vaultkeeper only on a
trusted host, restrict Docker access, and use encrypted host storage or an
encrypted volume when local-at-rest confidentiality is required. A crash can
leave a temporary export until the next run cleans that source directory.

The Auth configuration export can include SMTP or OAuth provider credentials.
Treat local staging, restore output, and the encrypted restic repository as
sensitive. Supabase Edge Function secrets are write-only and are not backed up.

## Restore safety

Database restores are destructive. Vaultkeeper requires an exact source-name
confirmation for every database restore and separately blocks targets sharing
the source host and port unless `--force-same-host` is supplied. PostgreSQL
restore cleanup is transactional; MongoDB restore uses `--drop --stopOnError`,
but MongoDB does not provide an equivalent all-or-nothing transaction for a
whole logical restore. Restore first into isolated infrastructure whenever
possible. Host-and-port comparison is a guardrail, not an identity proof: DNS
aliases and MongoDB SRV expansion can name the same server differently. The
typed source confirmation and an isolated recovery target remain required
operator checks.

Supabase Storage restore synchronizes the snapshot to the configured remote and
can delete remote objects that are absent from the snapshot. It requires the
explicit `--confirm-remote-overwrite` flag.

## Supply chain

CI audits Rust dependencies, scans Git history for secrets, and scans the built
container for fixed critical vulnerabilities. Release images include an SBOM
and build provenance. Base images, GitHub Actions, and downloaded release
artifacts are pinned; dependency findings still require ongoing review.

## Reporting a vulnerability

Use GitHub's private vulnerability reporting flow: open the repository's
Security tab and choose **Report a vulnerability**. Do not open a public issue
for an undisclosed vulnerability. We aim to acknowledge reports within one
week.
