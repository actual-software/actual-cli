# actual — Testing Guide

`actual` generates and maintains AI instruction files (`CLAUDE.md`, `AGENTS.md`, or Cursor rules) for your codebase by pulling tailored Architecture Decision Records from Actual's API and writing them into your repo.

## Prerequisites

You need an AI runner. The two easiest options:

**Option A — Claude Code CLI (default)**
Install the Claude Code CLI, then authenticate:
```bash
claude auth login
```

**Option B — Anthropic API key (no extra binary needed)**
```bash
export ANTHROPIC_API_KEY="sk-ant-..."
actual config set runner anthropic-api
```

## Running it

Place the `actual` binary somewhere on your PATH, then run it from inside any git repo:

```bash
actual sync
```

That's it. On first run it will analyze your repo, fetch relevant ADRs from Actual's API, tailor them to your codebase using the configured AI runner, and write the result to `CLAUDE.md` in your project root.

**Want to preview before writing anything?**
```bash
actual sync --dry-run
actual sync --dry-run --full   # also prints the full rendered output
```

## What you get

By default, `actual sync` writes to `CLAUDE.md`. Content is wrapped in managed markers so future runs update cleanly without touching anything you've written yourself. To write `AGENTS.md` instead:
```bash
actual sync --output-format agents-md
```

## Useful flags

| Flag | What it does |
|------|-------------|
| `--dry-run` | Preview only, no file changes |
| `--force` | Skip confirmations and cache |
| `--no-tui` | Plain text output (useful in CI or small terminals) |
| `--no-tailor` | Skip AI tailoring, use raw ADRs |
| `--verbose` | Show detailed progress |

## Check your setup

```bash
actual auth          # verify authentication
actual config show   # view current configuration
actual runners       # list available runners
```

Config lives at `~/.actualai/actual/config.yaml` and is created automatically on first run.
