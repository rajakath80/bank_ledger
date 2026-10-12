# Build context must be the WORKSPACE ROOT -- see ledger-guardian/Dockerfile's
# comment for why (Cargo workspace resolution needs every member present).
#
# This container needs Docker socket access, same as ledger-finops: its
# whole job is spawning the three agent containers as siblings on the
# HOST's Docker daemon (via `sh -c "docker run ..."`), not nested inside
# itself. GUARDIAN_CMD/TECHOPS_CMD/FINOPS_CMD's volume mounts (e.g.
# `-v /tmp/demo-ledger:/data:ro`) still refer to HOST paths even though
# they're launched from inside this container -- Docker always resolves
# mount sources against the daemon's host, regardless of which process
# issued the `docker run` command through the socket.
#
# Build:
#   docker build -f ledger-dashboard/Dockerfile -t ledger-dashboard:latest .
#
# Run (detached, stays up until you `docker stop` it; note OLLAMA_URL uses
# host.docker.internal, not localhost -- see main.rs's doc comment for why.
# LEDGER_DIR is mounted WRITABLE this time -- no `:ro` -- since this is the
# one place in the whole system that actually writes to the ledger):
#
#   docker run -d --name ledger-dashboard \
#     -p 3000:3000 \
#     -e GUARDIAN_CMD='docker run --rm -i -e LEDGER_AGENT_ROLE=guardian -v /tmp/demo-ledger:/data:ro ledger-guardian:latest /data' \
#     -e TECHOPS_CMD='docker run --rm -i -e LEDGER_AGENT_ROLE=techops -v /tmp/demo-ledger:/data:ro -v /tmp/demo-ledger-incidents:/incidents ledger-techops:latest /data /incidents' \
#     -e FINOPS_CMD='docker run --rm -i -e LEDGER_AGENT_ROLE=finops -v /tmp/demo-ledger:/data:ro -v /tmp/savings-tags.json:/tags.json:ro -v /var/run/docker.sock:/var/run/docker.sock ledger-finops:latest /data /tags.json' \
#     -e OLLAMA_URL='http://host.docker.internal:11434' \
#     -e LEDGER_DIR=/tmp/demo-ledger \
#     -v /tmp/demo-ledger:/tmp/demo-ledger \
#     -v /var/run/docker.sock:/var/run/docker.sock \
#     ledger-dashboard:latest
#
# Stop it:
#   docker stop ledger-dashboard && docker rm ledger-dashboard
#
# Logs (startup prints each agent's tool count, useful if something's wrong):
#   docker logs -f ledger-dashboard

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
COPY ledger-credit ./ledger-credit
COPY ledger-dashboard ./ledger-dashboard
COPY ledger-auth ./ledger-auth

RUN cargo build --release -p ledger-dashboard

# --- whisper.cpp, for fully local/offline speech-to-text ---
# A separate build stage, independent of the Rust builder above, so cmake/
# build-essential/git don't need to be installed into that image at all.
# `tiny.en` is used deliberately: this runs CPU-only inside the container
# (Docker Desktop's Linux VM has no access to Apple's Metal/Neural Engine,
# even on Apple Silicon), so the smallest usable model keeps latency
# tolerable. Swap to `base.en` (better accuracy, slower) if that tradeoff
# doesn't suit you -- just change the model name on both lines below.
FROM debian:bookworm AS whisper-builder
RUN apt-get update && apt-get install -y --no-install-recommends cmake build-essential git ca-certificates curl && rm -rf /var/lib/apt/lists/*
# Pinned to a tagged release rather than HEAD-of-main: more reproducible,
# and less exposed to whatever the latest ARM SIMD code churn introduces.
RUN git clone --depth 1 --branch v1.7.4 https://github.com/ggml-org/whisper.cpp.git /whisper.cpp
WORKDIR /whisper.cpp
# -DGGML_NATIVE=OFF: ggml's CMake build auto-detects -march=native and, on
# ARM64 (Apple Silicon under Docker's Linux VM), assumes FP16 arithmetic
# NEON intrinsics (vfmaq_f16, vaddq_f16) are safely usable -- but the
# compiler target flags this environment actually ends up with don't
# consistently support them, producing "target specific option mismatch"
# errors on `always_inline` functions. Disabling native detection falls
# back to a safe generic ARM baseline instead. This is a known class of
# issue with ggml/whisper.cpp on ARM64 Docker builds, not specific to this
# Dockerfile.
#
# -DBUILD_SHARED_LIBS=OFF: without this, whisper-cli links dynamically
# against libwhisper.so and ggml's own shared libraries, none of which
# exist in the slim runtime image below (only the compiled executable
# itself was ever going to be copied over) -- static linking makes
# whisper-cli self-contained, needing only the system libraries
# (libstdc++6, libgomp1) already installed there.
RUN cmake -B build -DGGML_NATIVE=OFF -DBUILD_SHARED_LIBS=OFF && cmake --build build --config Release -j
RUN ./models/download-ggml-model.sh tiny.en

FROM debian:bookworm-slim

# docker CLI to talk to the mounted socket -- see the module note above.
# libgomp1: whisper.cpp's default CPU build uses OpenMP for threading and
# doesn't run without it, unlike libstdc++6 which debian:bookworm-slim
# already carries as a base dependency.
RUN apt-get update && apt-get install -y --no-install-recommends docker.io ca-certificates libgomp1 && rm -rf /var/lib/apt/lists/*

COPY --from=builder /workspace/target/release/ledger-dashboard /usr/local/bin/ledger-dashboard
COPY --from=whisper-builder /whisper.cpp/build/bin/whisper-cli /usr/local/bin/whisper-cli
COPY --from=whisper-builder /whisper.cpp/models/ggml-tiny.en.bin /opt/whisper-model.bin

ENTRYPOINT ["/usr/local/bin/ledger-dashboard"]