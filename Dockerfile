# Multi-stage build for Flapjack server
FROM rust:1.91 as builder

WORKDIR /app

# Copy manifests
COPY Cargo.toml Cargo.lock ./

# Copy source code
COPY src ./src
COPY benches ./benches
COPY tests ./tests
COPY package ./package

# Build release binary
RUN cargo build --release --bin flapjack-server

# Runtime stage
FROM debian:bookworm-slim

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy binary from builder
COPY --from=builder /app/target/release/flapjack-server /usr/local/bin/flapjack-server

# Create data directory
RUN mkdir -p /data

# Expose default port
EXPOSE 7700

# Set working directory for data
WORKDIR /data

# Run the server
CMD ["flapjack-server"]
