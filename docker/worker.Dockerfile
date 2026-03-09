# =============================================================================
# Symphony Worker Dockerfile
# =============================================================================
# Multi-stage build for the Symphony worker process.
#
# The worker connects to the Symphony orchestrator, claims issues, and runs
# Claude Code sessions to implement changes. It needs a full development
# toolchain (Rust, git, gh CLI, Claude Code CLI) because it executes
# cargo commands and creates PRs.
#
# Build:
#   docker build -f docker/worker.Dockerfile -t symphony-worker .
#
# Run:
#   docker run \
#     -e ORCHESTRATOR_URL=http://host.docker.internal:8080 \
#     -e AUTH_TOKEN=<token> \
#     -e ANTHROPIC_API_KEY=<key> \
#     -e GITHUB_TOKEN=<token> \
#     -v ~/.ssh:/home/worker/.ssh:ro \
#     symphony-worker
#
# Required environment variables:
#   ORCHESTRATOR_URL  - URL of the Symphony orchestrator (e.g. http://host:8080)
#   AUTH_TOKEN        - Authentication token for the orchestrator API
#   ANTHROPIC_API_KEY - Anthropic API key for Claude Code sessions
#   GITHUB_TOKEN      - GitHub token for gh CLI (PR creation, CI checks)
#
# Optional environment variables:
#   LINEAR_API_KEY    - Linear API key for issue tracking
#   RUST_LOG          - Logging filter (default: info)
# =============================================================================

# ---------------------------------------------------------------------------
# Stage 1: Builder — compile the symphony-worker binary
# ---------------------------------------------------------------------------
FROM rust:1.87-bookworm AS builder

WORKDIR /build

# Copy workspace manifests first for better layer caching.
# If only source code changes, the dependency layer is reused.
COPY Cargo.toml Cargo.lock ./
COPY crates/symphony/Cargo.toml crates/symphony/Cargo.toml
COPY crates/symphony-worker/Cargo.toml crates/symphony-worker/Cargo.toml
COPY crates/tui-test/Cargo.toml crates/tui-test/Cargo.toml

# Create stub source files so cargo can resolve the workspace and fetch deps.
# The actual source is copied in the next step, invalidating only the build layer.
RUN mkdir -p src && echo "fn main() {}" > src/main.rs && \
    mkdir -p crates/symphony/src && echo "" > crates/symphony/src/lib.rs && \
    mkdir -p crates/symphony-worker/src && echo "fn main() {}" > crates/symphony-worker/src/main.rs && \
    mkdir -p crates/tui-test/src && echo "" > crates/tui-test/src/lib.rs

# Pre-fetch and compile dependencies (cached unless Cargo.toml/Cargo.lock change).
# We need a build.rs stub for the root crate if it has one.
COPY build.rs build.rs
RUN cargo build --release -p symphony-worker 2>/dev/null || true

# Now copy the real source code.
COPY src/ src/
COPY crates/ crates/
COPY build.rs build.rs

# Build the actual binary. Touch the main files to ensure cargo rebuilds them
# rather than using the stub artifacts.
RUN touch src/main.rs crates/symphony/src/lib.rs crates/symphony-worker/src/main.rs && \
    cargo build --release -p symphony-worker

# Verify the binary exists.
RUN test -f /build/target/release/symphony-worker

# ---------------------------------------------------------------------------
# Stage 2: Runtime — lightweight image with dev toolchain for worker tasks
# ---------------------------------------------------------------------------
FROM debian:bookworm-slim AS runtime

# Avoid interactive prompts during package installation.
ENV DEBIAN_FRONTEND=noninteractive

# Install runtime dependencies:
#   - ca-certificates: TLS for HTTPS connections
#   - git: clone repos, push branches
#   - openssh-client: SSH for git operations
#   - curl: general HTTP utility, used by installers
#   - build-essential: C compiler and friends (needed by some cargo builds)
#   - pkg-config: required by some Rust crates during compilation
#   - libssl-dev: OpenSSL headers (some crates need this at build time)
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        ca-certificates \
        git \
        openssh-client \
        curl \
        build-essential \
        pkg-config \
        libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# ---------------------------------------------------------------------------
# Install Rust toolchain via rustup (workers run cargo test, clippy, etc.)
# ---------------------------------------------------------------------------
ENV RUSTUP_HOME=/usr/local/rustup
ENV CARGO_HOME=/usr/local/cargo
ENV PATH="/usr/local/cargo/bin:${PATH}"

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
    sh -s -- -y --default-toolchain stable --profile default && \
    rustup component add clippy rustfmt && \
    # Verify installation
    rustc --version && \
    cargo --version && \
    clippy-driver --version

# ---------------------------------------------------------------------------
# Install GitHub CLI (gh) for PR creation and CI status checks
# ---------------------------------------------------------------------------
RUN curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
        -o /usr/share/keyrings/githubcli-archive-keyring.gpg && \
    chmod go+r /usr/share/keyrings/githubcli-archive-keyring.gpg && \
    echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
        > /etc/apt/sources.list.d/github-cli.list && \
    apt-get update && \
    apt-get install -y --no-install-recommends gh && \
    rm -rf /var/lib/apt/lists/* && \
    gh --version

# ---------------------------------------------------------------------------
# Install Node.js (required for Claude Code CLI)
# ---------------------------------------------------------------------------
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - && \
    apt-get install -y --no-install-recommends nodejs && \
    rm -rf /var/lib/apt/lists/* && \
    node --version && \
    npm --version

# ---------------------------------------------------------------------------
# Install Claude Code CLI
# ---------------------------------------------------------------------------
RUN npm install -g @anthropic-ai/claude-code && \
    claude --version

# ---------------------------------------------------------------------------
# Create non-root user for security
# ---------------------------------------------------------------------------
RUN groupadd --gid 1000 worker && \
    useradd --uid 1000 --gid worker --create-home --shell /bin/bash worker

# Create directories the worker will need.
RUN mkdir -p /home/worker/.ssh && \
    chown -R worker:worker /home/worker/.ssh && \
    chmod 700 /home/worker/.ssh

# Ensure cargo/rustup directories are accessible to the worker user.
RUN chmod -R a+r /usr/local/rustup && \
    chmod -R a+r /usr/local/cargo && \
    find /usr/local/cargo/bin -type f -exec chmod a+x {} +

# ---------------------------------------------------------------------------
# Copy the symphony-worker binary from the builder stage
# ---------------------------------------------------------------------------
COPY --from=builder /build/target/release/symphony-worker /usr/local/bin/symphony-worker
RUN chmod +x /usr/local/bin/symphony-worker

# ---------------------------------------------------------------------------
# Configure runtime environment
# ---------------------------------------------------------------------------
# Switch to non-root user.
USER worker
WORKDIR /home/worker

# Default log level.
ENV RUST_LOG=info

# SSH: accept host keys automatically (workers operate non-interactively).
RUN echo "Host *\n    StrictHostKeyChecking accept-new\n    UserKnownHostsFile ~/.ssh/known_hosts" \
    > /home/worker/.ssh/config && \
    chmod 600 /home/worker/.ssh/config

# No ports exposed — the worker is an outbound-only client that connects
# to the orchestrator via ORCHESTRATOR_URL.

ENTRYPOINT ["symphony-worker"]
