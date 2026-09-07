# Build context must be the WORKSPACE ROOT (the `ledger/` directory) --
# see ledger-guardian/Dockerfile's comment for why.
#
# Build:
#   docker build -f ledger-techops/Dockerfile -t ledger-techops:latest .
#
# Run (ledger dir read-only for traces, incidents dir read-write for
# create_incident; role must be set explicitly -- see the identical note
# in ledger-guardian/Dockerfile):
#   docker run --rm -i \
#     -e LEDGER_AGENT_ROLE=techops \
#     -v /tmp/demo-ledger:/data:ro \
#     -v /tmp/demo-ledger-incidents:/incidents \
#     ledger-techops:latest /data /incidents

FROM rust:1-bookworm AS builder
WORKDIR /workspace

# See the identical note in ledger-guardian/Dockerfile: every workspace
# member must be present for Cargo's resolution to succeed, even ones
# this specific binary doesn't depend on.
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

RUN cargo build --release -p ledger-techops

FROM debian:bookworm-slim
COPY --from=builder /workspace/target/release/ledger-techops /usr/local/bin/ledger-techops

ENTRYPOINT ["/usr/local/bin/ledger-techops"]