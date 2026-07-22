# syntax=docker/dockerfile:1
# Multi-stage build for Flapjack server.
# Build context must be the repository root (not engine/):
#   docker build -f engine/Dockerfile .

# ── Stage 1: Build dashboard ─────────────────────────────────────────────────
FROM node:20-alpine AS dashboard-builder
WORKDIR /app
COPY engine/dashboard/package.json engine/dashboard/package-lock.json ./
RUN npm ci
COPY engine/dashboard/ .
RUN npm run build

# ── Stage 2: Build Rust binary ───────────────────────────────────────────────
FROM rust:1 AS builder
WORKDIR /app

# Copy manifests
COPY engine/Cargo.toml engine/Cargo.lock ./

# Copy workspace crates
COPY engine/src ./src
COPY engine/flapjack-http ./flapjack-http
COPY engine/flapjack-server ./flapjack-server
COPY engine/flapjack-replication ./flapjack-replication
COPY engine/flapjack-ssl ./flapjack-ssl
COPY engine/benches ./benches
COPY engine/tests ./tests
COPY engine/package ./package

# Copy built dashboard (required by RustEmbed)
COPY --from=dashboard-builder /app/dist ./dashboard/dist
ENV FLAPJACK_REQUIRE_DASHBOARD=1

# rust:1 is based on Debian trixie (GLIBC 2.41, GCC 14). usearch/simsimd requires
# GLIBC_2.39+ and CXXABI_1.3.15 (GCC 14) at runtime, so the runtime image must
# match trixie. vector-search is enabled here (unlike the musl release builds).
RUN cargo build --release --features vector-search -p flapjack-server

# ── Stage 3: Runtime ─────────────────────────────────────────────────────────
# Must match the builder (rust:1 = trixie) to satisfy GLIBC_2.39 and CXXABI_1.3.15.
FROM debian:trixie-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3t64 \
    libstdc++6 \
    curl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/flapjack /usr/local/bin/flapjack
COPY engine/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

RUN mkdir -p /data

EXPOSE 7700

WORKDIR /data

ENTRYPOINT ["entrypoint.sh"]
# Container-only default: bind to all interfaces so bare `docker run -p 7700:7700`
# is reachable from the host. Native builds keep the loopback-only default in
# resolve_bind_addr (engine/flapjack-server/src/main.rs). Compose files may
# override this via their own FLAPJACK_BIND_ADDR environment setting.
ENV FLAPJACK_BIND_ADDR=0.0.0.0:7700
CMD ["flapjack", "--data-dir", "/data"]
