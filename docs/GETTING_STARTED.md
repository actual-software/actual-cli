# actual — Getting Started

`actual` generates and maintains AI instruction files (`CLAUDE.md`, `AGENTS.md`, or Cursor rules) for your codebase by pulling tailored Architecture Decision Records from Actual's API and writing them into your repo.

## Installation

### Homebrew (recommended)

```bash
brew install actual-software/actual/actual
```

### npm

```bash
npm install -g @actualai/actual
```

### Manual download

Download the binary for your platform, then:

```bash
chmod +x ./actual
sudo mv ./actual /usr/local/bin/actual
```

**macOS only:** remove quarantine before running:

```bash
xattr -dr com.apple.quarantine /usr/local/bin/actual
```

Verify it works: `actual --version`

## Prerequisites

You need an AI runner. Pick one:

**Claude Code CLI (default)**
```bash
claude auth login
```

**Anthropic API**
```bash
export ANTHROPIC_API_KEY="sk-ant-..."
actual config set runner anthropic-api
```

**OpenAI API**
```bash
export OPENAI_API_KEY="sk-..."
actual config set runner openai-api
```

**Codex CLI**
```bash
# Requires the `codex` binary on your PATH
actual config set runner codex-cli
```

**Cursor CLI**
```bash
# Requires the `agent` binary on your PATH
actual config set runner cursor-cli
```

## Running it

From inside any git repo:

```bash
actual adr-bot
```

On first run it will analyze your repo, fetch relevant ADRs from Actual's API, tailor them to your codebase using the configured AI runner, and write the result to `CLAUDE.md` in your project root.

**Preview before writing anything:**
```bash
actual adr-bot --dry-run
actual adr-bot --dry-run --full   # also prints the full rendered output
```

## Output formats

By default, `actual adr-bot` writes to `CLAUDE.md`. Content is wrapped in managed markers so future runs update cleanly without touching anything you've written yourself.

| Format | Flag | Output file |
|--------|------|-------------|
| Claude Code (default) | `--output-format claude-md` | `CLAUDE.md` |
| Agents | `--output-format agents-md` | `AGENTS.md` |
| Cursor Rules | `--output-format cursor-rules` | `.cursor/rules/actual-policies.mdc` |

## Flags

| Flag | What it does |
|------|-------------|
| `--dry-run` | Preview only, no file changes |
| `--full` | With `--dry-run`, output the full rendered file to stdout |
| `--force` | Skip confirmations and bypass all caches |
| `--no-tui` | Plain text output (useful in CI or small terminals) |
| `--no-tailor` | Skip AI tailoring, use raw ADRs |
| `--verbose` | Show detailed progress |
| `--runner <RUNNER>` | AI backend: `claude-cli`, `anthropic-api`, `openai-api`, `codex-cli`, `cursor-cli` |
| `--model <MODEL>` | Override the model used for tailoring |
| `--output-format <FMT>` | Output format (see table above) |
| `--project <PATH>` | Target a sub-project in a monorepo (repeatable) |
| `--max-budget-usd <USD>` | Max budget per tailoring invocation |
| `--reset-rejections` | Clear remembered ADR rejections |
| `--show-errors` | Stream runner subprocess stderr in real time |
| `--api-url <URL>` | Override ADR bank API endpoint |

## Commands

```bash
actual adr-bot        # analyze repo & write AI context files
actual status         # check output file state (managed markers, staleness)
actual auth           # verify authentication
actual config show    # view current configuration
actual config set     # set a config value (e.g. actual config set runner anthropic-api)
actual config path    # print config file location
actual runners        # list available AI backend runners
actual models         # list known model names grouped by runner
actual cache clear    # clear local analysis and tailoring caches
```

## Configuration

Config lives at `~/.actualai/actual/config.yaml` and is created automatically on first run.

Override the config location with environment variables:

| Variable | Effect |
|----------|--------|
| `ACTUAL_CONFIG` | Exact path to the config file |
| `ACTUAL_CONFIG_DIR` | Directory containing `config.yaml` |
