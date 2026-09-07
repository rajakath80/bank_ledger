# Build context must be the WORKSPACE ROOT -- see ledger-guardian/Dockerfile's
# comment for why (Cargo workspace resolution needs every member present).
#
# Build:
#   docker build -f ledger-finops/Dockerfile -t ledger-finops:latest .
#
# Run -- NOTE the docker socket mount: get_infrastructure_cost/
# calculate_unit_cost need this server to query the HOST's Docker daemon
# for other containers' stats, which means giving it the socket. This is
# effectively root-equivalent access to the host: anything with the
# socket can control every container and image on the machine. Standard
# practice for tooling like this, but know what you're granting.
#
#   docker run --rm -i \
#     -e LEDGER_AGENT_ROLE=finops \
#     -v /tmp/demo-ledger:/data:ro \
#     -v /path/to/tags.json:/tags.json:ro \
#     -v /var/run/docker.sock:/var/run/docker.sock \
#     ledger-finops:latest /data /tags.json

FROM rust:1-bookworm AS builder
WORKDIR /workspace

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

RUN cargo build --release -p ledger-finops

FROM debian:bookworm-slim

# The docker CLI binary itself -- needed to shell out `docker stats`
# against the mounted socket. This does NOT run a Docker daemon inside
# the container; it's just the client executable talking to the host's
# daemon through the socket.
RUN apt-get update && apt-get install -y --no-install-recommends docker.io && rm -rf /var/lib/apt/lists/*

COPY --from=builder /workspace/target/release/ledger-finops /usr/local/bin/ledger-finops

ENTRYPOINT ["/usr/local/bin/ledger-finops"]