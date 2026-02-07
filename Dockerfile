# Multi-stage build for Flapjack server
FROM rust:1.85 as builder

WORKDIR /app

# Copy workspace manifests first for better layer caching
COPY Cargo.toml Cargo.lock ./
COPY flapjack-http/Cargo.toml flapjack-http/
COPY flapjack-server/Cargo.toml flapjack-server/
COPY flapjack-ssl/Cargo.toml flapjack-ssl/
COPY flapjack-replication/Cargo.toml flapjack-replication/

# Copy source code
COPY src ./src
COPY flapjack-http/src ./flapjack-http/src
COPY flapjack-server/src ./flapjack-server/src
COPY flapjack-ssl/src ./flapjack-ssl/src
COPY flapjack-replication/src ./flapjack-replication/src

# Build release binary
RUN cargo build --release --package flapjack-server

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy binary from builder
COPY --from=builder /app/target/release/flapjack-server /usr/local/bin/flapjack-server

# Create data directory
RUN mkdir -p /data

EXPOSE 7701

WORKDIR /data

CMD ["flapjack-server"]