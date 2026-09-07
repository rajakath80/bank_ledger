# Build context must be the WORKSPACE ROOT (the `ledger/` directory), not
# this crate's own directory: path dependencies (ledger-core, ledger-log,
# ledger-store, ledger-types) require their sibling crate directories to
# be present wherever the build runs.
#
# Build:
#   docker build -f ledger-guardian/Dockerfile -t ledger-guardian:latest .
#
# Run (ledger data mounted read-only; role must be set explicitly --
# docker does NOT inherit your shell's environment automatically):
#   docker run --rm -i \
#     -e LEDGER_AGENT_ROLE=guardian \
#     -v /tmp/demo-ledger:/data:ro \
#     ledger-guardian:latest /data

FROM rust:1-bookworm AS builder
WORKDIR /workspace

# Cargo's workspace resolution requires every member listed in the root
# Cargo.toml to be present and readable -- even ones this specific binary
# doesn't depend on -- or resolution fails before `-p` narrows the build.
# So all eight crates get copied here, not just ledger-guardian's own
# dependency chain.
COPY Cargo.toml Cargo.lock ./
COPY ledger-types ./ledger-types
COPY ledger-core ./ledger-core
COPY ledger-log ./ledger-log
COPY ledger-store ./ledger-store
COPY ledger-guardian ./ledger-guardian
COPY ledger-agent ./ledger-agent
COPY ledger-techops ./ledger-techops
COPY ledger-tags ./ledger-tags
COPY ledger-finops ./ledger-finops
COPY ledger-dashboard ./ledger-dashboard

RUN cargo build --release -p ledger-guardian

FROM debian:bookworm-slim
COPY --from=builder /workspace/target/release/ledger-guardian /usr/local/bin/ledger-guardian

# Role is intentionally NOT baked in here -- see main.rs's check_role().
# The same image refuses to run under any role but "guardian" unless
# LEDGER_AGENT_ROLE is set correctly at `docker run` time.
ENTRYPOINT ["/usr/local/bin/ledger-guardian"]