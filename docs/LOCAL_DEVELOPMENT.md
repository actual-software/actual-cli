# Local Development Guide

This guide covers how to build the `actual` CLI from source, run the full
verification suite locally, and iterate confidently before opening a PR.

## Prerequisites

- **Rust** (stable toolchain) — install via [rustup](https://rustup.rs/):
  ```bash
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```
- **Clippy and rustfmt** ship with the default toolchain; verify they're present:
  ```bash
  rustup component add clippy rustfmt
  ```
- **Git**

## Clone and build

```bash
git clone https://github.com/actual-software/actual-cli.git
cd actual-cli

# Debug build (fast, not optimized)
cargo build

# Release build (optimized, stripped, LTO enabled — matches the distributed binary)
cargo build --release
```

Binaries land in:

| Build type | Path |
|---|---|
| Debug | `target/debug/actual` |
| Release | `target/release/actual` |

## Run the locally built binary

Point your shell at the debug binary during development so you don't have to
reinstall on every change:

```bash
# From the repo root, run without installing
./target/debug/actual --version

# Or add target/debug to your PATH for the session
export PATH="$PWD/target/debug:$PATH"
actual --version
```

For a quick smoke-test against a real repo:

```bash
# Dry-run in this repo (no files written, no API calls to runners)
./target/debug/actual adr-bot --dry-run
```

## Verification checklist

Run these four commands before every commit or push. They match exactly what CI
enforces:

### 1. Format

```bash
cargo fmt --check
```

Fix any formatting issues automatically:

```bash
cargo fmt
```

### 2. Lint

```bash
cargo clippy -- -D warnings
```

All clippy warnings are treated as errors. Fix each one before moving on.

### 3. Tests

```bash
# Unit tests only (fast, no external dependencies)
cargo test --workspace

# Unit tests + integration tests
cargo test --workspace --features integration
```

Integration tests spin up a mock HTTP server (via `mockito`) and exercise the
full CLI pipeline end-to-end without hitting the real API. They are gated
behind `--features integration` to keep the default `cargo test` cycle fast.

### 4. Coverage (optional but useful before PRs)

CI enforces 100% per-file line coverage. Check coverage locally with the same
exclusions CI uses:

```bash
cargo install cargo-llvm-cov   # one-time install

cargo llvm-cov --workspace \
  --ignore-filename-regex '(src/main\.rs|tests/|real_terminal\.rs|sync_kb_poller\.rs|tui/renderer\.rs|pty\.rs|session\.rs|test_support\.rs)' \
  --lcov --output-path lcov.info
```

For an HTML report you can open in a browser:

```bash
cargo llvm-cov --workspace \
  --ignore-filename-regex '(src/main\.rs|tests/|real_terminal\.rs|sync_kb_poller\.rs|tui/renderer\.rs|pty\.rs|session\.rs|test_support\.rs)' \
  --html --open
```

Any file that drops below 100% will be highlighted. Add tests to cover the
missing lines before submitting.

## Run the full check suite in one shot

```bash
cargo fmt --check \
  && cargo clippy -- -D warnings \
  && cargo test --workspace --features integration
```

If all three pass, your change is ready to push.

## E2E tests (CI only, requires live credentials)

End-to-end tests in `scripts/e2e.sh` run against the real API and require a
`CLAUDE_CODE_OAUTH_TOKEN` and a live API endpoint. These run automatically in
the merge queue — you don't need to run them locally unless you're debugging
a scenario that only reproduces against the real API.

```bash
# Build a release binary first
cargo build --release

# Then run (requires credentials)
CLAUDE_CODE_OAUTH_TOKEN=<token> \
ACTUAL_E2E_API_URL=<api-url> \
  scripts/e2e.sh ./target/release/actual
```

## Iterating quickly

- Use `cargo check` to get compiler feedback without a full build — it's
  significantly faster than `cargo build`:
  ```bash
  cargo check
  ```
- Pass a specific test name to avoid running the full suite:
  ```bash
  cargo test --workspace test_name_fragment
  ```
- Add `-- --nocapture` to see `println!` / `tracing` output from tests:
  ```bash
  cargo test --workspace --features integration -- --nocapture
  ```
- Set `RUST_LOG=debug` to enable tracing output from the binary itself:
  ```bash
  RUST_LOG=debug ./target/debug/actual adr-bot --dry-run
  ```

## Project layout reference

```
src/
├── analysis/      # Static code analysis (language detection, frameworks)
├── api/           # HTTP client for the Actual API
├── branding/      # CLI branding and terminal output
├── cli/           # Command definitions and argument parsing
├── config/        # Configuration loading and paths
├── generation/    # Output file generation (CLAUDE.md, AGENTS.md, etc.)
├── runner/        # AI provider runners (Anthropic, OpenAI, Claude Code, etc.)
├── tailoring/     # ADR tailoring via LLM
├── telemetry/     # Anonymous usage metrics
├── main.rs        # Entry point
├── lib.rs         # Library root
├── error.rs       # Error types
├── model_cache.rs # OpenAI/Anthropic model list caching
└── testutil.rs    # Shared test utilities
crates/
└── tui-test/      # TUI E2E testing library (PTY-based screenshot comparison)
tests/             # Integration tests
scripts/           # E2E and helper scripts
docs/              # Documentation
```

See [CONTRIBUTING.md](../CONTRIBUTING.md) for coding standards and PR
submission guidelines.
