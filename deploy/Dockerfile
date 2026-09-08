# Multi-stage Dockerfile for INCLINE Watershed Delineation System
# ================================================================
# Builder Stage
FROM rust:1.85-bookworm AS builder
WORKDIR /app

# Copy dependency specifications for layer caching
COPY Cargo.toml Cargo.lock ./
COPY crates/core/Cargo.toml crates/core/

# Dummy build to cache dependencies
RUN mkdir -p src crates/core/src && \
    echo "fn main() {}" > src/main.rs && \
    echo "pub fn dummy() {}" > crates/core/src/lib.rs && \
    cargo build --release || true && \
    rm -rf src crates/core/src

# Copy complete project source
COPY . .

# Build release binary
RUN cargo build --release --bin incline-watershed-system

# ================================================================
# Production Runtime Stage
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /opt/incline

# Copy compiled binary from builder
COPY --from=builder /app/target/release/incline-watershed-system /usr/local/bin/incline-watershed-system
COPY data/ ./data/

# Environment configurations
ENV PORT=8787
ENV HOST=0.0.0.0
ENV INCLINE_DATASET_PATH=/data/grit-hfx-v0.3.0

EXPOSE 8787

ENTRYPOINT ["incline-watershed-system"]
CMD ["--dataset", "/data/grit-hfx-v0.3.0", "--port", "8787", "--host", "0.0.0.0"]
