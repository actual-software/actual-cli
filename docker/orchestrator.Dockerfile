# syntax=docker/dockerfile:1

# =============================================================================
# Stage 1: Builder
# =============================================================================
FROM rust:1-bookworm AS builder

WORKDIR /build

# Copy workspace manifest and lockfile first (for layer caching)
COPY Cargo.toml Cargo.lock ./

# Copy the build script (referenced by root package)
COPY build.rs ./

# Copy all workspace crate manifests
COPY crates/symphony/Cargo.toml crates/symphony/Cargo.toml
COPY crates/symphony-worker/Cargo.toml crates/symphony-worker/Cargo.toml
COPY crates/tui-test/Cargo.toml crates/tui-test/Cargo.toml

# Copy root package source (workspace member ".")
COPY src/ src/

# Copy all crate sources
COPY crates/ crates/

# Build only the symphony binary in release mode
# rusqlite "bundled" feature compiles SQLite from C source, so a C compiler
# is needed here but NOT in the runtime image.
RUN cargo build --release -p symphony

# =============================================================================
# Stage 2: Runtime
# =============================================================================
FROM debian:bookworm-slim AS runtime

# Install runtime dependencies:
#   - ca-certificates: HTTPS/TLS connections
#   - git: workspace hooks clone repositories
#   - openssh-client: git SSH operations
#   - bash: agent subprocesses are spawned via `bash -lc`
#   (bash is included in bookworm-slim, but we list it explicitly for clarity)
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        git \
        openssh-client \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user
RUN groupadd --system symphony \
    && useradd --system --gid symphony --create-home symphony

# Create data directory for SQLite persistence
RUN mkdir -p /data && chown symphony:symphony /data

# Copy the built binary from the builder stage
COPY --from=builder /build/target/release/symphony /usr/local/bin/symphony

# Metadata labels
LABEL maintainer="actual-software" \
      org.opencontainers.image.title="Symphony Orchestrator" \
      org.opencontainers.image.description="Long-running service that orchestrates coding agents to work on issues from Linear" \
      org.opencontainers.image.source="https://github.com/actual-software/actual-cli"

# Expose the default dashboard/API port
EXPOSE 7070

# Switch to non-root user
USER symphony

# Health check using the liveness endpoint
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -f http://localhost:7070/healthz || exit 1

# Default entrypoint; bind to 0.0.0.0 so the server is accessible outside the
# container (the default 127.0.0.1 would not be reachable from the host).
# WORKFLOW.md and environment variables (LINEAR_API_KEY, GITHUB_TOKEN,
# ANTHROPIC_API_KEY) must be provided at runtime.
ENTRYPOINT ["symphony"]
CMD ["--bind", "0.0.0.0:7070"]
