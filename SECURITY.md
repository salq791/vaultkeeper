# Security Policy

Vaultkeeper stores database credentials encrypted at rest (ChaCha20-Poly1305,
key from the VAULTKEEPER_MASTER_KEY environment variable) and passes secrets
to child processes only via environment variables.

## Reporting a vulnerability

Please open a GitHub security advisory (Security tab > Report a vulnerability)
rather than a public issue. You will get a response within a week.
