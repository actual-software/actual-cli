import { NextResponse } from "next/server";

const LLMS_TXT = `# actual — AI context for your codebase

> actual is a CLI tool that analyzes a git repository, fetches the team's Architectural Decision Records (ADRs) from the Actual AI bank, tailors them to the detected languages and frameworks, and writes AI context files — keeping every coding agent architecturally aligned, automatically.

Install via Homebrew: \`brew install actual-software/actual/actual\`
Zero-install: \`npx actual adr-bot\`

Output formats: CLAUDE.md (claude-md), AGENTS.md (agents-md), .cursor/rules/actual-policies.mdc (cursor-rules)

Supported AI backends (runners): claude-cli (default), anthropic-api, openai-api, codex-cli, cursor-cli

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
