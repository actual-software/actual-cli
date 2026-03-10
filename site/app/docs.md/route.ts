import { NextResponse } from "next/server";
import {
    ADR_BOT_FLAGS,
    ADR_BOT_EXAMPLES,
    RUNNERS,
    OUTPUT_FORMATS,
    CONFIG_KEYS,
    CONFIG_EXAMPLES,
    ENV_VARS,
    COMMON_ERRORS,
    EXIT_CODES,
    SKILL_INSTALL_METHODS,
} from "../../lib/docs-data";

function buildFlagsTable(flags: { flag: string; type: string; desc: string }[]): string {
    const rows = flags.map((f) => `| ${f.flag} | ${f.type} | ${f.desc} |`).join("\n");
    return `| Flag | Type | Description |\n|------|------|-------------|\n${rows}`;
}

function buildRunnersTable(runners: { name: string; req: string; model: string; setup: string }[]): string {
    const rows = runners
        .map((r) => `| ${r.name === "claude-cli" ? `${r.name} (default)` : r.name} | ${r.req} | ${r.model} | ${r.setup.replace(/\n/g, " ")} |`)
        .join("\n");
    return `| Runner | Requires | Default Model | Setup |\n|--------|----------|---------------|-------|\n${rows}`;
}

function buildOutputFormatsTable(formats: { value: string; file: string; tool: string }[]): string {
    const rows = formats
        .map((f) => `| ${f.value === "claude-md" ? `${f.value} (default)` : f.value} | ${f.file} | ${f.tool} |`)
        .join("\n");
    return `| Value | Output File | Tool |\n|-------|-------------|------|\n${rows}`;
}

function buildConfigKeysTable(keys: { key: string; default_: string; desc: string }[]): string {
    const rows = keys.map((k) => `| ${k.key} | ${k.default_} | ${k.desc} |`).join("\n");
    return `| Key | Default | Description |\n|-----|---------|-------------|\n${rows}`;
}

function buildEnvVarsTable(vars: { name: string; purpose: string }[]): string {
    const rows = vars.map((v) => `| ${v.name} | ${v.purpose} |`).join("\n");
    return `| Variable | Purpose |\n|----------|--------|\n${rows}`;
}

function buildErrorsTable(errors: { err: string; fix: string }[]): string {
    const rows = errors.map((e) => `| ${e.err} | ${e.fix} |`).join("\n");
    return `| Error | Fix |\n|-------|-----|\n${rows}`;
}

function buildExitCodesTable(codes: { code: string; meaning: string }[]): string {
    const rows = codes.map((c) => `| ${c.code} | ${c.meaning} |`).join("\n");
    return `| Code | Meaning |\n|------|--------|\n${rows}`;
}

const DOCS_MD = `# actual CLI — Getting Started & Command Reference

> actual is a CLI tool that analyzes your git repository, fetches your team's Architectural Decision Records (ADRs), tailors them to your detected languages and frameworks, and writes AI context files — keeping every coding agent architecturally aligned, automatically.

## Quick Start

\`\`\`bash
# 1. Install
brew install actual-software/actual/actual
# or zero-install (Node required):
npx @actualai/actual adr-bot

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

${buildFlagsTable(ADR_BOT_FLAGS)}

Examples:

\`\`\`bash
${ADR_BOT_EXAMPLES.map((e) => `# ${e.comment}\n${e.cmd}`).join("\n\n")}
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

${buildConfigKeysTable(CONFIG_KEYS)}

Examples:

\`\`\`bash
${CONFIG_EXAMPLES.join("\n")}
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

${buildRunnersTable(RUNNERS)}

Runner auto-detection (when no --runner specified):
- Anthropic short aliases (sonnet, opus, haiku) → claude-cli, then anthropic-api
- Full claude-* model IDs → anthropic-api, then claude-cli
- gpt-*, o1*, o3*, o4*, chatgpt-* → codex-cli, then openai-api
- codex-*, gpt-*-codex* → codex-cli

Note: Short aliases only work with claude-cli. anthropic-api requires full model names (e.g. claude-sonnet-4-6).

---

## Output Formats

${buildOutputFormatsTable(OUTPUT_FORMATS)}

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

## AI Skill

The actual AI skill teaches coding agents how to run, configure, and troubleshoot the CLI. Install it alongside the CLI so your AI assistant can operate actual autonomously.

The skill covers: sync workflow, runner selection, error diagnosis, and configuration management.

### Install methods

${SKILL_INSTALL_METHODS.map((m) => `**${m.name}** (${m.tool})\n\n\`\`\`bash\n${m.command}\n\`\`\`\n\n${m.note}`).join("\n\n---\n\n")}

Source: [actual-software/actual-skill](https://github.com/actual-software/actual-skill) (Apache-2.0) | [actual-software/actual-skill-openclaw](https://github.com/actual-software/actual-skill-openclaw) (MIT-0, ClawdHub)

---

## Configuration

Config file: ~/.actualai/actual/config.yaml (auto-created, mode 0600 on Unix)

Environment variables:

${buildEnvVarsTable(ENV_VARS)}

---

## Troubleshooting

Common errors and fixes:

${buildErrorsTable(COMMON_ERRORS)}

Debug flags:

\`\`\`bash
actual adr-bot --verbose --show-errors   # full runner output + stderr
actual adr-bot --no-tui                  # plain output, good for CI
RUST_LOG=debug actual adr-bot --no-tui 2>&1
\`\`\`

Exit codes:

${buildExitCodesTable(EXIT_CODES)}`.trim();

export async function GET() {
    return new NextResponse(DOCS_MD, {
        headers: {
            "Content-Type": "text/markdown; charset=utf-8",
            "Cache-Control": "public, max-age=3600",
        },
    });
}
