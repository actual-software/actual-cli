---
name: actual
description: >-
  Feature-complete companion for the actual CLI, an ADR-powered
  CLAUDE.md/AGENTS.md generator. Runs and troubleshoots actual sync,
  status, auth, config, runners, and models. Covers all 5 runners
  (claude-cli, anthropic-api, openai-api, codex-cli, cursor-cli),
  all model patterns, all 3 output formats (claude-md, agents-md,
  cursor-rules), and all error types. Use when working with the
  actual CLI, running actual sync, configuring runners or models,
  troubleshooting errors, or managing output files.
argument-hint: "[question or command] e.g. 'run sync', 'set up anthropic-api', 'fix ClaudeNotFound'"
---

# actual CLI Companion

Inline knowledge and operational workflows for the actual CLI. Read this file first; load reference files only when you need deeper detail for a specific topic.

## Commands

| Command | Purpose | Key Flags |
|---------|---------|-----------|
| `actual sync` | Analyze repo, fetch ADRs, tailor, write output | `--dry-run [--full]`, `--force`, `--no-tailor`, `--project PATH`, `--model`, `--runner`, `--verbose`, `--reset-rejections`, `--max-budget-usd`, `--no-tui`, `--output-format`, `--show-errors` |
| `actual status` | Check output file state | `--verbose` |
| `actual auth` | Check authentication status | (none) |
| `actual config show` | Display current config | (none) |
| `actual config set <key> <value>` | Set a config value | (none) |
| `actual config path` | Print config file path | (none) |
| `actual runners` | List available runners | (none) |
| `actual models` | List known models by runner | (none) |

## Runner Decision Tree

Use this to determine which runner a user needs:

```
Has claude binary installed?
  YES -> claude-cli (default runner, no API key needed)
  NO  -> Do they want Anthropic models?
           YES -> anthropic-api (needs ANTHROPIC_API_KEY)
           NO  -> Do they want OpenAI models?
                    YES -> codex-cli or openai-api (needs OPENAI_API_KEY)
                    NO  -> cursor-cli (needs agent binary, optional CURSOR_API_KEY)
```

### Runner Summary

| Runner | Binary | Auth | Default Model |
|--------|--------|------|---------------|
| claude-cli | `claude` | `claude auth login` | claude-sonnet-4-6 |
| anthropic-api | (none) | `ANTHROPIC_API_KEY` | claude-sonnet-4-6 |
| openai-api | (none) | `OPENAI_API_KEY` | gpt-5.2 |
| codex-cli | `codex` | `OPENAI_API_KEY` or `codex login` (ChatGPT OAuth) | gpt-5.2 |
| cursor-cli | `agent` | Optional `CURSOR_API_KEY` | (cursor default) |

### Model-to-Runner Inference

The CLI auto-selects a runner from the model name:

| Model Pattern | Inferred Runner |
|---------------|-----------------|
| `sonnet`, `opus`, `haiku` (short aliases) | claude-cli |
| `claude-*` (full names) | anthropic-api |
| `gpt-*`, `o1*`, `o3*`, `o4*`, `chatgpt-*` | codex-cli |
| `codex-*`, `gpt-*-codex*` | codex-cli |
| Unrecognized | Error with suggestions |

> For deep runner details (install steps, compatibility, special behaviors), see `references/runner-guide.md`.

## Sync Quick Reference

The most common sync patterns:

```bash
# Preview what sync would do (safe, no file changes)
actual sync --dry-run

# Preview with full content
actual sync --dry-run --full

# Run sync, skip confirmation prompts
actual sync --force

# Sync specific subdirectories only (monorepo)
actual sync --project services/api --project services/web

# Use a specific runner/model
actual sync --runner anthropic-api --model claude-sonnet-4-6

# Skip AI tailoring (use raw ADRs)
actual sync --no-tailor

# Re-offer previously rejected ADRs
actual sync --reset-rejections

# Set spending cap
actual sync --max-budget-usd 5.00
```

> For the complete 13-step sync internals, see `references/sync-workflow.md`.

## Operational Workflow: Running Sync

Follow this pattern whenever running sync. Do NOT skip pre-flight.

### 1. Pre-flight (LOW freedom -- exact commands)

```bash
actual runners          # Verify runner is available
actual auth             # Verify authentication (for claude-cli)
actual config show      # Review current configuration
```

If any check shows a problem, diagnose and fix before proceeding.

### 2. Dry-run (LOW freedom -- exact command)

```bash
actual sync --dry-run [--full] [user's flags]
```

Show the user what would change. Let them review.

### 3. Confirm (HIGH freedom)

Ask user if they want to proceed. If no, stop.

### 4. Execute (LOW freedom -- exact command)

```bash
actual sync [user's flags]
```

### 5. On failure: Diagnose

Match the error against the troubleshooting table below. For full error details, load `references/error-catalog.md`.

### 6. Fix and retry

Apply the fix, then return to step 1 to verify.

## Operational Workflow: Diagnostics

For comprehensive environment checks, run the bundled diagnostic script:

```bash
bash .claude/skills/actual/scripts/diagnose.sh
```

This checks all binaries, auth status, environment variables, config, and output files in one pass. It is read-only and never modifies anything.

Use inline commands instead when checking a single thing (e.g., just `actual auth`).

## Troubleshooting Quick Reference

| Error | Exit Code | Likely Cause | Quick Fix |
|-------|-----------|-------------|-----------|
| ClaudeNotFound | 2 | `claude` binary not in PATH | Install Claude Code CLI |
| ClaudeNotAuthenticated | 2 | Not logged in | Run `claude auth login` |
| CodexNotFound | 2 | `codex` binary not in PATH | Install Codex CLI |
| CodexNotAuthenticated | 2 | No auth for codex | Set `OPENAI_API_KEY` or run `codex login` |
| CursorNotFound | 2 | `agent` binary not in PATH | Install Cursor CLI |
| ApiKeyMissing | 2 | Required env var not set | Set `ANTHROPIC_API_KEY` or `OPENAI_API_KEY` |
| CodexCliModelRequiresApiKey | 2 | ChatGPT OAuth with explicit model | Set `OPENAI_API_KEY` (OAuth only supports default model) |
| CreditBalanceTooLow | 3 | Insufficient API credits | Add credits to account |
| ApiError | 3 | API request failed | Check API URL, network, credentials |
| ApiResponseError | 3 | Unexpected API response | Check API status, retry |
| RunnerFailed | 1 | Runner process errored | Check runner output, logs |
| RunnerOutputParse | 1 | Could not parse runner output | Check model compatibility |
| RunnerTimeout | 1 | Runner exceeded time limit | Increase `invocation_timeout_secs` |
| ConfigError | 1 | Invalid config file | Check YAML syntax, run `actual config show` |
| AnalysisEmpty | 1 | No analysis results | Check project path, repo content |
| TailoringValidationError | 1 | Tailored output invalid | Retry, or use `--no-tailor` |
| IoError | 5 | File I/O failure | Check permissions, disk space |
| UserCancelled | 4 | User cancelled operation | (intentional) |

> For full error details with hints and diagnosis steps, see `references/error-catalog.md`.

## Exit Code Categories

| Code | Category | Errors |
|------|----------|--------|
| 1 | General / runtime | RunnerFailed, RunnerOutputParse, ConfigError, RunnerTimeout, AnalysisEmpty, TailoringValidationError, InternalError, TerminalIOError |
| 2 | Auth / setup | ClaudeNotFound, ClaudeNotAuthenticated, CodexNotFound, CodexNotAuthenticated, CursorNotFound, ApiKeyMissing, CodexCliModelRequiresApiKey |
| 3 | Billing / API | CreditBalanceTooLow, ApiError, ApiResponseError |
| 4 | User cancelled | UserCancelled |
| 5 | I/O | IoError |

## Config Quick Reference

Config file: `~/.actualai/actual/config.yaml` (override with `ACTUAL_CONFIG` or `ACTUAL_CONFIG_DIR` env vars).

Most-used config keys:

| Key | Default | Purpose |
|-----|---------|---------|
| `runner` | claude-cli | Which runner to use |
| `model` | claude-sonnet-4-6 | Model for Anthropic runners |
| `output_format` | claude-md | Output format: claude-md, agents-md, cursor-rules |
| `batch_size` | 15 | ADRs per batch (min 1) |
| `concurrency` | 10 | Parallel requests (min 1) |
| `invocation_timeout_secs` | 600 | Runner timeout in seconds (min 1) |
| `max_budget_usd` | (none) | Spending cap (positive, finite) |

> For all 18 config keys with validation rules, see `references/config-reference.md`.

## Output Formats

| Format | File | Header |
|--------|------|--------|
| claude-md (default) | `CLAUDE.md` | `# Project Guidelines` |
| agents-md | `AGENTS.md` | `# Project Guidelines` |
| cursor-rules | `.cursor/rules/actual-policies.mdc` | YAML frontmatter (`alwaysApply: true`) |

Managed sections use markers: `<!-- managed:actual-start -->` / `<!-- managed:actual-end -->`.

Merge behavior:
- **New root file**: header + managed section
- **New subdir file**: managed section only (no header)
- **Existing with markers**: replace between markers, preserve surrounding content
- **Existing without markers**: append managed section

> For full format details and merge internals, see `references/output-formats.md`.

## Reference Files

Load these only when you need deeper detail on a specific topic:

| File | When to Load |
|------|-------------|
| `references/sync-workflow.md` | Debugging sync failures, understanding sync internals |
| `references/runner-guide.md` | Setting up a runner, model compatibility, runner-specific behavior |
| `references/error-catalog.md` | Troubleshooting a specific error with full diagnosis steps |
| `references/config-reference.md` | Looking up config keys, validation rules, dotpath syntax |
| `references/output-formats.md` | Output format questions, managed section behavior, merge logic |
