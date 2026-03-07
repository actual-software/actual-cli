---
tracker:
  kind: linear
  team_key: ACTCLI
  api_key: $LINEAR_API_KEY
  active_states: "Todo, In Progress, In Review"
  terminal_states: "Merged, Done, Closed, Cancelled, Canceled, Duplicate"
polling:
  interval_ms: 30000
workspace:
  root: /tmp/actual-cli-ws
agent:
  max_concurrent_agents: 3
  max_turns: 30
  max_retry_backoff_ms: 300000
  max_concurrent_agents_by_state:
    todo: 2
    in progress: 3
    in review: 1
coding_agent:
  command: "claude -p --output-format stream-json --verbose --dangerously-skip-permissions"
  stall_timeout_ms: 600000
  turn_timeout_ms: 1800000
server:
  port: 7070
hooks:
  # NOTE: Hooks are executed as raw shell scripts (bash -lc). They do NOT
  # support Liquid template syntax. The workspace directory name is the
  # sanitized issue identifier (e.g., ACTCLI-42), available via $(basename $(pwd)).
  after_create: |
    # Clone the repo into the empty workspace directory
    git clone --branch main --depth 1 git@github.com:actual-software/actual-cli.git .
    git fetch --unshallow origin main
    # Install Rust toolchain components needed for quality gates
    rustup component add clippy rustfmt llvm-tools-preview
  before_run: |
    ISSUE_KEY=$(basename "$(pwd)")
    BRANCH="symphony/$(echo "$ISSUE_KEY" | tr '[:upper:]' '[:lower:]')"
    git fetch origin main
    # Preserve existing branch work on retries; only create if new
    git checkout "$BRANCH" 2>/dev/null || git checkout -b "$BRANCH" origin/main
    # Fast-forward to latest main if no local commits yet
    LOCAL_COMMITS=$(git rev-list "origin/main..$BRANCH" --count 2>/dev/null || echo "0")
    if [ "$LOCAL_COMMITS" = "0" ]; then
      git reset --hard origin/main
    fi
  timeout_ms: 300000
---

## First Steps (MANDATORY)

Before doing ANY work, read the project's agent instructions:

```bash
cat CLAUDE.md
cat AGENTS.md
```

Follow all conventions, workflows, and rules described in those files.

## Your Task

You are an expert Rust engineer working on **actual-cli**, an ADR-powered AI context file generator built in Rust. You are working on issue **{{ issue.identifier }}: {{ issue.title }}**.

{% if issue.url %}
Issue URL: {{ issue.url }}
{% endif %}

{% if issue.description %}
## Issue Description

{{ issue.description }}
{% endif %}

{% if issue.labels.size > 0 %}
**Labels**: {% for label in issue.labels %}{{ label }}{% unless forloop.last %}, {% endunless %}{% endfor %}
{% endif %}

{% if issue.blocked_by.size > 0 %}
**WARNING**: This issue has {{ issue.blocked_by.size }} blockers:
{% for blocker in issue.blocked_by %}
- {{ blocker.identifier }} ({{ blocker.state }})
{% endfor %}
Check if these are resolved before proceeding.
{% endif %}

{% if attempt %}
## Retry Context

This is retry attempt #{{ attempt }}. A previous attempt failed. Before starting fresh:
1. Check `git log` for any commits from the previous attempt
2. Run `cargo test` to see the current state
3. Review what was done previously and continue from where it left off
{% endif %}

## Repository Overview

- **Language**: Rust (workspace with multiple crates)
- **Binary**: `actual` — generates CLAUDE.md/AGENTS.md files from ADRs
- **Crates**: `crates/symphony/` (orchestrator service), `crates/tui-test/` (TUI testing library)
- **Key directories**: `src/` (main binary), `tests/` (integration tests), `docs/` (planning docs)

## Quality Gates (MANDATORY)

You MUST pass ALL of these before committing. Run them in this order:

```bash
cargo fmt --check       # Fix: cargo fmt
cargo clippy -- -D warnings  # Zero warnings allowed
cargo test              # Zero failures
cargo build --release   # Must compile
```

If any gate fails, fix it. Do NOT commit or push broken code.

## Coverage Requirement

This project enforces **100% per-file line coverage** via CI. Every source file (except `main.rs` and a few excluded files) must have 100% test coverage. When adding new code:

1. Write tests for every code path, including error branches
2. Check coverage locally if `cargo-llvm-cov` is installed:
   ```bash
   cargo llvm-cov --workspace --lcov --output-path lcov.info
   ```
3. If a line is uncovered, add a test that exercises it

## Git Workflow

All changes MUST go through a PR. Never push directly to `main`.

```bash
# 1. Create a feature branch (the before_run hook already does this)
# 2. Make your changes, commit with descriptive messages
git add .
git commit -m "{{ issue.identifier }}: <description of what changed>"

# 3. Rebase onto latest main before pushing
git fetch origin main && git rebase origin/main

# 4. Push and create PR
git push -u origin "symphony/{{ issue.identifier | downcase }}"
gh pr create \
  --title "{{ issue.identifier }}: {{ issue.title }}" \
  --body "Resolves {{ issue.identifier }}"

# 5. Wait for CI and verify all checks pass
gh pr checks $(gh pr view --json number -q .number) --watch

# 6. Read and address ALL review comments (from Claude, bots, or humans)
PR_NUM=$(gh pr view --json number -q .number)
gh api repos/actual-software/actual-cli/pulls/$PR_NUM/comments --jq '.[] | {user: .user.login, body: .body, path: .path}'
gh api repos/actual-software/actual-cli/issues/$PR_NUM/comments --jq '.[] | {user: .user.login, body: .body}'
gh pr view $PR_NUM --json reviews --jq '.reviews[] | select(.body != "") | {user: .author.login, body: .body}'

# 7. If feedback exists: fix, push, re-check CI, re-read comments
#    Repeat until all checks pass AND all feedback is addressed
```

## PR Requirements

- **Title format**: `ACTCLI-<N>: <description>` (use the Linear issue identifier)
- **Branch format**: `symphony/<identifier>` (created by the before_run hook)
- **All CI checks must be green** before the PR is considered complete
- **All code review feedback must be addressed** — fix issues, push, re-verify
- PRs go through a **merge queue** with rebase strategy
- Do NOT merge PRs — the orchestrator or a human handles merging

## Refactoring for Testability

When code can't be tested due to external dependencies:

1. Create traits for external dependencies
2. Use dependency injection (accept trait objects or generics)
3. Use `mockall` for mocks in tests
4. Test error paths, not just happy paths

## Key Conventions

- Use `thiserror` for error types
- Use `tracing` for structured logging (not `println!`)
- Use `serde` for serialization
- Use `tokio` for async runtime
- Prefer `anyhow::Result` in binary code, typed errors in library code
- Tests go in the same file as the code they test (`#[cfg(test)] mod tests`)
- Integration tests go in `tests/`
