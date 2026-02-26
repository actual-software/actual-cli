import { NextResponse } from "next/server";

const INDEX_MD = `# actual — AI context for your codebase

> actual is a CLI tool that analyzes a git repository, fetches the team's Architectural Decision Records (ADRs) from the Actual AI bank, tailors them to the detected languages and frameworks, and writes AI context files — keeping every coding agent architecturally aligned, automatically.

## How It Works

1. **Install** — via Homebrew or zero-install with npx:
   \`\`\`bash
   brew install actual-software/actual/actual
   # or zero-install (no Gatekeeper issues — runs through Node.js):
   npx @actualai/actual adr-bot
   \`\`\`

2. **Authenticate** — actual uses Claude Code as the default AI backend:
   \`\`\`bash
   npm install -g @anthropic-ai/claude-code
   claude auth login
   actual auth   # verify
   \`\`\`

3. **Run** — from any git repository:
   \`\`\`bash
   actual adr-bot
   \`\`\`
   actual analyzes your codebase, fetches matching ADRs, tailors them to your stack, and writes the output file. Run it again whenever your architecture evolves.

## Supported Output Formats

| Value | Output File | Tool |
|-------|-------------|------|
| claude-md (default) | CLAUDE.md | Claude Code |
| agents-md | AGENTS.md | Codex CLI, OpenCode |
| cursor-rules | .cursor/rules/actual-policies.mdc | Cursor IDE |

Choose with \`--output-format\` or \`actual config set output_format\`.

## Supported Languages & Frameworks

- **TypeScript** — Next.js, HeroUI
- **Rust** — Ratatui
- **Python** — FastAPI, Django
- **Java** — Spring Boot

More languages and frameworks are being added continuously.

## Full Documentation

Complete command reference, runner setup, output formats, configuration, and troubleshooting:
https://cli.actual.ai/docs.md
`.trim();

export async function GET() {
    return new NextResponse(INDEX_MD, {
        headers: {
            "Content-Type": "text/markdown; charset=utf-8",
            "Cache-Control": "public, max-age=3600",
        },
    });
}
