# actual

Guide AI coding agents with tailored software development best practices.

[![CI](https://github.com/actual-software/actual-cli/actions/workflows/build-and-test.yml/badge.svg)](https://github.com/actual-software/actual-cli/actions/workflows/build-and-test.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

## What it does

Actual CLI establishes guardrails for AI coding agents by delivering
Architecture Decision Records (ADRs) — short documents that capture
software development best practices and design guidance — tailored to your
specific codebase using an LLM. This catches edge cases and guides agents
toward correct decisions. The output is written to context files
(`CLAUDE.md`, `AGENTS.md`, or Cursor rules) that agents read automatically.

The Actual API is **free and requires no account or API key**. You only need
a configured AI runner (Claude Code, Anthropic API, OpenAI API, etc.) for
the tailoring step.

## Quick start

1. **Install**

   ```bash
   brew install actual-software/actual/actual
   ```

2. **Configure a runner** (Claude Code CLI is the default)

   ```bash
   claude auth login
   ```

3. **Run it** from inside any git repo

   ```bash
   actual adr-bot
   ```

This analyzes your repo, fetches relevant ADRs, tailors them to your
codebase, and writes the result to `CLAUDE.md`.

Preview before writing anything:

```bash
actual adr-bot --dry-run
```

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

Download the binary for your platform from the
[releases repository](https://github.com/actual-software/actual-releases/releases)
(binaries are published separately from source), then:

```bash
chmod +x ./actual
sudo mv ./actual /usr/local/bin/actual
```

**macOS only:** remove quarantine before running:

```bash
xattr -dr com.apple.quarantine /usr/local/bin/actual
```

### Build from source

Requires a C compiler (for tree-sitter native dependencies):

```bash
# macOS: Xcode command-line tools (usually already installed)
xcode-select --install

# Debian/Ubuntu:
sudo apt-get install build-essential

# Then:
cargo install --git https://github.com/actual-software/actual-cli.git
```

## Supported platforms

| Platform | Architecture | Install method |
|----------|-------------|----------------|
| macOS | Apple Silicon (arm64) | Homebrew, npm, manual download |
| macOS | Intel (x64) | Homebrew, npm, manual download |
| Linux | x64 | npm, manual download |
| Linux | arm64 | npm, manual download |

Windows is not currently supported.

## Output formats

By default, `actual adr-bot` writes to `CLAUDE.md`. Content is wrapped in
managed markers so future runs update cleanly without touching anything
you've written yourself.

| Format | Flag | Output file |
|--------|------|-------------|
| Claude Code (default) | `--output-format claude-md` | `CLAUDE.md` |
| Agents | `--output-format agents-md` | `AGENTS.md` |
| Cursor Rules | `--output-format cursor-rules` | `.cursor/rules/actual-policies.mdc` |

## Commands

```
actual adr-bot        # analyze repo & write AI context files
actual status         # check output file state (managed markers, staleness)
actual auth           # verify authentication
actual config show    # view current configuration
actual config set     # set a config value
actual config path    # print config file location
actual runners        # list available AI backend runners
actual models         # list known model names grouped by runner
actual cache clear    # clear local analysis and tailoring caches
```

## Configuration

Config lives at `~/.actualai/actual/config.yaml` and is created automatically
on first run. See [Getting Started](docs/GETTING_STARTED.md) for the full
reference including all flags, runner configuration, and environment variable
overrides.

## Privacy & Telemetry

Actual CLI collects minimal, anonymous telemetry (aggregate counters only —
no PII, source code, or file paths). Telemetry can be disabled via
environment variable (`ACTUAL_NO_TELEMETRY=1`), config file, or compile-time
feature flag. See [PRIVACY.md](PRIVACY.md) for full details.

## Contributing

Contributions are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for build
instructions, coding standards, and PR guidelines.

## Security

To report a vulnerability, **do not open a public issue**. See
[SECURITY.md](SECURITY.md) for responsible disclosure instructions.

## License

[MIT](LICENSE)
