import { NextResponse } from "next/server";

const LLMS_TXT = `# actual — AI context for your codebase

> actual is a CLI tool that analyzes a git repository, fetches the team's Architectural Decision Records (ADRs) from the Actual AI bank, tailors them to the detected languages and frameworks, and writes AI context files — keeping every coding agent architecturally aligned, automatically.

Install via Homebrew: \`brew install actual-software/actual/actual\`
Zero-install: \`npx @actualai/actual adr-bot\`

Output formats: CLAUDE.md (claude-md), AGENTS.md (agents-md), .cursor/rules/actual-policies.mdc (cursor-rules)

Supported AI backends (runners): claude-cli (default), anthropic-api, openai-api, codex-cli, cursor-cli

## Commands

- \`actual adr-bot\` — Analyze repo, fetch ADRs, tailor, and write the output file (main command)
- \`actual auth\` — Check Claude Code authentication status
- \`actual status\` — Show current config and state of managed output files in the current repo
- \`actual config\` — View or edit the global config file (~/.actualai/actual/config.yaml)
- \`actual runners\` — List all available AI backends and their current availability status
- \`actual models\` — List known model names grouped by runner
- \`actual cache\` — Clear local analysis and tailoring cache

## Key Flags (actual adr-bot)

- \`--dry-run\` — Preview changes without writing files
- \`--force\` — Skip confirmation prompts and bypass all caches
- \`--output-format <FMT>\` — claude-md (default) | agents-md | cursor-rules
- \`--runner <RUNNER>\` — AI backend: claude-cli | anthropic-api | openai-api | codex-cli | cursor-cli
- \`--model <MODEL>\` — Override AI model; runner is auto-inferred from the model name
- \`--project <PATH>\` — Target a sub-project in a monorepo (repeatable)
- \`--no-tailor\` — Write raw ADRs without AI tailoring

## Runners

- \`claude-cli\` (default) — requires the claude binary; install: \`npm install -g @anthropic-ai/claude-code && claude auth login\`
- \`anthropic-api\` — requires ANTHROPIC_API_KEY; set: \`export ANTHROPIC_API_KEY=sk-ant-...\`
- \`openai-api\` — requires OPENAI_API_KEY; set: \`export OPENAI_API_KEY=sk-...\`
- \`codex-cli\` — requires the codex binary; install: \`npm install -g @openai/codex && codex login\`
- \`cursor-cli\` — requires the agent binary; install: \`curl https://cursor.com/install -fsS | bash\`

## Output Formats

- \`claude-md\` → CLAUDE.md (Claude Code)
- \`agents-md\` → AGENTS.md (Codex CLI, OpenCode)
- \`cursor-rules\` → .cursor/rules/actual-policies.mdc (Cursor IDE)

## Docs

- [Getting Started & Full Command Reference](https://cli.actual.ai/docs.md): Installation, quick start, all commands with flags, runners, output formats, configuration, and troubleshooting

## Optional

- [Product overview](https://cli.actual.ai/index.md): Homepage — tagline, install instructions, how it works
- [actual.ai](https://actual.ai): Main product site with pricing, about, blog, and agent documentation
`.trim();

export async function GET() {
    return new NextResponse(LLMS_TXT, {
        headers: {
            "Content-Type": "text/plain; charset=utf-8",
            "Cache-Control": "public, max-age=3600",
        },
    });
}
