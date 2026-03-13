# Contributing to Actual CLI

Thank you for your interest in contributing to Actual CLI. This document
covers the development workflow, coding standards, and how to submit changes.

## Getting Started

### Prerequisites

- **Rust** (stable toolchain) — install via [rustup](https://rustup.rs/)
- **Clippy and rustfmt** — installed with the default Rust toolchain
- **Git** — for version control

### Building

```bash
# Debug build
cargo build

# Release build (with LTO + stripping)
cargo build --release
```

The binary is named `actual` and lives in `target/debug/actual` or
`target/release/actual`.

### Running Tests

```bash
# Unit tests + integration tests
cargo test --workspace --features integration
```

Integration tests are gated behind the `integration` feature flag. Without
it, only unit tests run.

### Linting and Formatting

```bash
# Check formatting (must pass before committing)
cargo fmt --check

# Run clippy (all warnings are errors)
cargo clippy -- -D warnings
```

The project uses default `rustfmt` and `clippy` settings — no custom
configuration files.

### Coverage

CI enforces **100% per-file line coverage** for most source files (a small
set of files is excluded — see `.github/workflows/coverage.yml`). To check
coverage locally with the same exclusions as CI:

```bash
cargo install cargo-llvm-cov
cargo llvm-cov --workspace \
  --ignore-filename-regex '(src/main\.rs|tests/|real_terminal\.rs|sync_kb_poller\.rs|tui/renderer\.rs|pty\.rs|session\.rs|test_support\.rs)' \
  --lcov --output-path lcov.info
```

## Project Structure

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
```

## Coding Standards

- Use `thiserror` for error types
- Use `tracing` for structured logging (not `println!`)
- Use `serde` for serialization
- Use `tokio` for the async runtime
- Prefer `anyhow::Result` in binary code, typed errors in library code
- Unit tests go in the same file as the code they test
  (`#[cfg(test)] mod tests`)
- Integration tests go in `tests/`

## Submitting Changes

1. **Fork the repository** and create a branch from `main`
2. **Make your changes** — keep commits focused and atomic
3. **Run the full check suite locally** before pushing:
   ```bash
   cargo fmt --check
   cargo clippy -- -D warnings
   cargo test --workspace --features integration
   ```
4. **Open a pull request** targeting `main`

### What happens in CI

Every PR runs these checks:

| Check | What it does |
|-------|-------------|
| **Lint** | `cargo fmt --check` + `cargo clippy -- -D warnings` |
| **Build** | Release builds for x86_64 and aarch64 Linux |
| **Test** | `cargo test --workspace --features integration` |
| **Coverage** | 100% per-file line coverage enforcement |

PRs enter a **merge queue** before landing on `main`. The merge queue also
runs live E2E tests against the real API.

### PR Guidelines

- Keep PRs small and focused — one logical change per PR
- Include tests for new functionality
- Update documentation if behavior changes
- Make sure all CI checks pass before requesting review

## Reporting Bugs

Open a [GitHub issue](https://github.com/actual-software/actual-cli/issues)
with:

- Steps to reproduce
- Expected vs actual behavior
- CLI version (`actual --version`)
- OS and architecture

## Security Vulnerabilities

Do **not** open a public issue for security vulnerabilities. See
[SECURITY.md](SECURITY.md) for reporting instructions.

## License

By contributing, you agree that your contributions will be licensed under
the [MIT License](LICENSE).
