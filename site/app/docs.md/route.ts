import { NextResponse } from "next/server";

const DOCS_MD = `# actual CLI — Getting Started & Command Reference

> actual is a CLI tool that analyzes your git repository, fetches your team's Architectural Decision Records (ADRs), tailors them to your detected languages and frameworks, and writes AI context files — keeping every coding agent architecturally aligned, automatically.

## Quick Start

\`\`\`bash
# 1. Install
brew install actual-software/actual/actual
# or zero-install (no Gatekeeper issues — runs through Node.js):
npx actual adr-bot

# macOS Gatekeeper: the binary is not codesigned yet (Apple Developer Program application
# pending). After brew install, remove the quarantine flag once before running:
xattr -dr com.apple.quarantine $(which actual)
# Alternatively, use "npx actual adr-bot" — it runs through Node.js and bypasses Gatekeeper.

# 2. Authenticate (default runner: Claude Code)
npm install -g @anthropic-ai/claude-code
claude auth login
actual auth   # verify

# 3. Sync — run from any git repo
actual adr-bot
\`\`\`

---

## Commands

### actual adr-bot

Analyze repo, fetch ADRs, tailor, and write the output file.

\`\`\`
actual adr-bot [flags]
\`\`\`

Flags:

| Flag | Type | Description |
|------|------|-------------|
| --dry-run | bool | Preview changes without writing files |
| --full | bool | With --dry-run, print full rendered file to stdout |
| --force | bool | Skip confirmation prompts and bypass all caches |
| --no-tailor | bool | Write raw ADRs without AI tailoring |
| --project <PATH> | string | Target a sub-project in a monorepo (repeatable) |
| --output-format <FMT> | enum | claude-md (default) \| agents-md \| cursor-rules |
| --runner <RUNNER> | enum | claude-cli \| anthropic-api \| openai-api \| codex-cli \| cursor-cli |
| --model <MODEL> | string | Override AI model; runner is auto-inferred |
| --max-budget-usd <N> | float | Spending cap per tailoring invocation (USD) |
| --reset-rejections | bool | Clear remembered ADR rejections |
| --verbose | bool | Show detailed progress and runner output |
| --show-errors | bool | Stream runner stderr in real time |
| --no-tui | bool | Disable TUI; use plain line output |
| --api-url <URL> | string | Override the ADR bank API endpoint |

Examples:

\`\`\`bash
actual adr-bot --dry-run
actual adr-bot --force
actual adr-bot --output-format agents-md
actual adr-bot --runner anthropic-api
actual adr-bot --model gpt-5.2
actual adr-bot --project packages/api --project packages/web
actual adr-bot --verbose --show-errors
\`\`\`

---

### actual auth

Check Claude Code authentication status.

\`\`\`bash
actual auth
\`\`\`

Prints authenticated status or an error with a fix hint.

---

### actual status

Show current config and state of managed output files in the current repo.

\`\`\`bash
actual status
actual status --verbose   # also shows runner, cached analysis, ADR counts
\`\`\`

---

### actual config

View or edit the global config file (~/.actualai/actual/config.yaml).

\`\`\`bash
actual config show              # print config (API keys redacted)
actual config path              # print config file location
actual config set <KEY> <VALUE> # set a config value
\`\`\`

Settable keys:

| Key | Default | Description |
|-----|---------|-------------|
| runner | claude-cli | AI backend: claude-cli \| anthropic-api \| openai-api \| codex-cli \| cursor-cli |
| model | — | Default model (runner auto-inferred from name) |
| output_format | claude-md | claude-md \| agents-md \| cursor-rules |
| batch_size | 15 | ADRs per API request batch |
| concurrency | 10 | Max concurrent API requests |
| invocation_timeout_secs | 600 | Runner timeout in seconds |
| max_budget_usd | — | Spending cap per tailoring invocation |
| max_turns | — | Max conversation turns (claude-cli only) |
| max_per_framework | — | Max ADRs per detected framework |
| include_general | — | Include language-agnostic ADRs |
| include_categories | — | Only include these ADR categories (comma-separated) |
| exclude_categories | — | Exclude these ADR categories (comma-separated) |
| anthropic_api_key | — | Anthropic API key (stored at mode 0600) |
| openai_api_key | — | OpenAI API key |
| cursor_api_key | — | Cursor API key |
| telemetry.enabled | true | Enable or disable telemetry |

Examples:

\`\`\`bash
actual config set runner anthropic-api
actual config set model claude-sonnet-4-6
actual config set output_format agents-md
actual config set invocation_timeout_secs 1200
actual config set telemetry.enabled false
actual config set anthropic_api_key "sk-ant-..."
\`\`\`

---

### actual runners

List all available AI backends and their current availability status.

\`\`\`bash
actual runners
\`\`\`

---

### actual models

List known model names grouped by runner. Fetches live model lists from Anthropic and OpenAI by default.

\`\`\`bash
actual models
actual models --no-fetch   # skip live fetch; show hardcoded list only
\`\`\`

---

### actual cache

Clear local analysis and tailoring cache.

\`\`\`bash
actual cache clear
\`\`\`

Cache entries expire automatically after 7 days. Pass --force to actual adr-bot to bypass cache without clearing.

---

## Runners

A runner is the AI backend actual uses to tailor ADRs to your codebase.

| Runner | Requires | Default Model | Setup |
|--------|----------|---------------|-------|
| claude-cli (default) | claude binary | claude-sonnet-4-6 | npm install -g @anthropic-ai/claude-code && claude auth login |
| anthropic-api | ANTHROPIC_API_KEY | claude-sonnet-4-6 | export ANTHROPIC_API_KEY=sk-ant-... |
| openai-api | OPENAI_API_KEY | gpt-5.2 | export OPENAI_API_KEY=sk-... |
| codex-cli | codex binary | gpt-5.2-codex | npm install -g @openai/codex && codex login |
| cursor-cli | agent binary | opus-4.6-thinking | curl https://cursor.com/install -fsS \| bash |

Runner auto-detection (when no --runner specified):
- Anthropic short aliases (sonnet, opus, haiku) → claude-cli, then anthropic-api
- Full claude-* model IDs → anthropic-api, then claude-cli
- gpt-*, o1*, o3*, o4*, chatgpt-* → codex-cli, then openai-api
- codex-*, gpt-*-codex* → codex-cli

Note: Short aliases only work with claude-cli. anthropic-api requires full model names (e.g. claude-sonnet-4-6).

---

## Output Formats

| Value | Output File | Tool |
|-------|-------------|------|
| claude-md (default) | CLAUDE.md | Claude Code |
| agents-md | AGENTS.md | Codex CLI, OpenCode |
| cursor-rules | .cursor/rules/actual-policies.mdc | Cursor IDE |

All formats use managed-section markers. Content outside the markers is always preserved:

\`\`\`
<!-- managed:actual-start -->
<!-- last-synced: 2025-01-15T10:30:00Z -->
<!-- version: 3 -->
<!-- adr-ids: adr-001,adr-002 -->
...tailored ADR content...
<!-- managed:actual-end -->
\`\`\`

---

## Configuration

Config file: ~/.actualai/actual/config.yaml (auto-created, mode 0600 on Unix)

Environment variables:

| Variable | Purpose |
|----------|---------|
| ACTUAL_CONFIG | Exact path to config file (highest precedence) |
| ACTUAL_CONFIG_DIR | Directory for config (must be absolute) |
| ANTHROPIC_API_KEY | Anthropic API key (overrides config) |
| OPENAI_API_KEY | OpenAI API key (overrides config) |
| CURSOR_API_KEY | Cursor API key (overrides config) |
| CLAUDE_BINARY | Override path to the claude binary |
| RUST_LOG | Log level (default: warn) |

---

## Troubleshooting

Common errors and fixes:

| Error | Fix |
|-------|-----|
| Claude Code is not installed | npm install -g @anthropic-ai/claude-code |
| Claude Code is not authenticated | claude auth login |
| No runner available for model '…' | Install a runner or set an API key; run actual runners |
| Runner timed out after 600s | actual config set invocation_timeout_secs 1200 |
| Insufficient credits | Add credits at provider billing page |
| Analysis returned no projects | Run from inside a git repo; use --project <PATH> for monorepos |

Debug flags:

\`\`\`bash
actual adr-bot --verbose --show-errors   # full runner output + stderr
actual adr-bot --no-tui                  # plain output, good for CI
RUST_LOG=debug actual adr-bot --no-tui 2>&1
\`\`\`

Exit codes:

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | General runtime error |
| 2 | Auth / setup error |
| 3 | Billing / API error |
| 4 | User cancelled |
| 5 | I/O error |
`.trim();

export async function GET() {
    return new NextResponse(DOCS_MD, {
        headers: {
            "Content-Type": "text/markdown; charset=utf-8",
            "Cache-Control": "public, max-age=3600",
        },
    });
}
