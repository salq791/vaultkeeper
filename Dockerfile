FROM rust:1-bookworm AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY tests ./tests
RUN cargo build --release

FROM debian:bookworm-slim
# supabase release assets embed the version in the filename, so the CLI must
# be pinned; latest/download/supabase_linux_amd64.deb does not exist (404)
ARG SUPABASE_CLI_VERSION=2.109.1
LABEL org.opencontainers.image.source="https://github.com/salq791/vaultkeeper"
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl gnupg \
    && install -d /usr/share/postgresql-common/pgdg \
    && curl -fsSL -o /usr/share/postgresql-common/pgdg/apt.postgresql.org.asc https://www.postgresql.org/media/keys/ACCC4CF8.asc \
    && echo "deb [signed-by=/usr/share/postgresql-common/pgdg/apt.postgresql.org.asc] https://apt.postgresql.org/pub/repos/apt bookworm-pgdg main" > /etc/apt/sources.list.d/pgdg.list \
    && curl -fsSL https://www.mongodb.org/static/pgp/server-8.0.asc | gpg --dearmor -o /usr/share/keyrings/mongodb-server-8.0.gpg \
    && echo "deb [signed-by=/usr/share/keyrings/mongodb-server-8.0.gpg] https://repo.mongodb.org/apt/debian bookworm/mongodb-org/8.0 main" > /etc/apt/sources.list.d/mongodb-org-8.0.list \
    && apt-get update \
    && apt-get install -y --no-install-recommends postgresql-client-18 mongodb-database-tools restic rclone \
    && curl -fsSL -o /tmp/supabase.deb "https://github.com/supabase/cli/releases/download/v${SUPABASE_CLI_VERSION}/supabase_${SUPABASE_CLI_VERSION}_linux_amd64.deb" \
    && apt-get install -y --no-install-recommends /tmp/supabase.deb \
    && rm -f /tmp/supabase.deb \
    && apt-get purge -y gnupg curl \
    && apt-get autoremove -y \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /src/target/release/vaultkeeper /usr/local/bin/vaultkeeper

RUN useradd --uid 1000 --create-home vaultkeeper \
    && mkdir -p /config /data /staging \
    && chown vaultkeeper:vaultkeeper /data /staging

USER vaultkeeper
VOLUME ["/data", "/staging"]
ENTRYPOINT ["/usr/local/bin/vaultkeeper"]
CMD ["daemon"]
