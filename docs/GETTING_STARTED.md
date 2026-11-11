# actual — Getting Started

`actual` generates and maintains AI instruction files (`CLAUDE.md`, `AGENTS.md`, or Cursor rules) for your codebase by pulling tailored Architecture Decision Records from Actual's API and writing them into your repo.

## Installation

**Linux:** every method below installs a prebuilt binary that needs glibc 2.39 or newer: Ubuntu 24.04, Debian 13, Fedora 40, RHEL 10, or a later release of any of them. Check yours with `ldd --version`. Ubuntu 22.04, Debian 12, RHEL 9 and Amazon Linux 2023 ship an older glibc, where the arm64 binary exits with ``version `GLIBC_2.39' not found`` and the x64 binary runs but prints ``weak version `GLIBC_2.39' not found`` every time it starts. On any glibc older than 2.39, build from source instead with `cargo install --git https://github.com/actual-software/actual-cli.git actual-cli`, which needs a Rust toolchain and a C compiler.

### Homebrew (recommended)

```bash
brew install actual-software/actual/actual
```

### npm

```bash
npm install -g @actualai/actual
```

### Manual download

Download the archive for your platform from the [releases repository](https://github.com/actual-software/actual-releases/releases): `actual-darwin-arm64.tar.gz`, `actual-darwin-x64.tar.gz`, `actual-linux-arm64.tar.gz`, or `actual-linux-x64.tar.gz`. Extract it and move the binary onto your `PATH`. On Apple Silicon:

```bash
curl -fsSLO https://github.com/actual-software/actual-releases/releases/latest/download/actual-darwin-arm64.tar.gz
tar -xzf actual-darwin-arm64.tar.gz
sudo mv ./actual /usr/local/bin/actual
```

**macOS only:** if you downloaded the archive in a browser, remove quarantine before running:

```bash
xattr -dr com.apple.quarantine /usr/local/bin/actual
```

Verify it works: `actual --version`

`actual self-update` upgrades a binary installed this way after checking the new release's signature, but only in a directory you can write without `sudo`. To use it, put `actual` somewhere you own, such as `~/.local/bin`, instead of `/usr/local/bin`. See [SELF_UPDATE.md](SELF_UPDATE.md).

### Check a downloaded release

From v0.12.0, the release workflow publishes `SHA256SUMS`, which lists every archive, on both the [source release page](https://github.com/actual-software/actual-cli/releases) and the [releases mirror](https://github.com/actual-software/actual-releases/releases). A signer adds `SHA256SUMS.minisig`, a [minisign](https://jedisct1.github.io/minisign/) signature over that list, by hand after publication. A new release can remain unsigned until that step finishes; if the signature is missing, try again later. Releases through v0.11.0 carry neither file. For a signed release, check the signature first:

```bash
curl -fsSLO https://github.com/actual-software/actual-releases/releases/latest/download/SHA256SUMS
curl -fsSLO https://github.com/actual-software/actual-releases/releases/latest/download/SHA256SUMS.minisig
curl -fsSL -o actual-release.pub https://raw.githubusercontent.com/actual-software/actual-cli/main/release-keys/primary.pub
minisign -Vm SHA256SUMS -p actual-release.pub
shasum -a 256 --check --ignore-missing SHA256SUMS
```

`minisign` then prints the signed trusted comment, which should name the tag of the release you downloaded, because a signature from an older release still verifies. If it reports a different key id, check against the spare key in [Release signing](RELEASE_SIGNING.md).

### Build from source

To build from source you need a C compiler for the tree-sitter native dependencies:

```bash
# macOS: Xcode command-line tools (usually already installed)
xcode-select --install

# Debian/Ubuntu
sudo apt-get install build-essential

cargo install --git https://github.com/actual-software/actual-cli.git actual-cli
```

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
# Requires `cursor-agent` on your PATH (falls back to `agent` if not found)
actual config set runner cursor-cli
```

## Commands

```bash
actual adr-bot        # analyze repo & write AI context files
actual advisor        # ask the Advisor an org-scoped architecture question
actual status         # check output file state (managed markers, staleness)
actual auth           # verify the configured AI runner is authenticated
actual config show    # view current configuration
actual config set     # set a config value (e.g. actual config set runner anthropic-api)
actual config path    # print config file location
actual runners        # list available AI backend runners
actual models         # list known model names grouped by runner
actual cache clear    # clear local analysis and tailoring caches
actual version        # print the version and how to upgrade it (--check compares with the latest)
```

## Configuration

Config lives at `~/.actualai/actual/config.yaml` and is created automatically
on first run. All fields are optional — missing fields use sensible defaults.

### Config file keys

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `api_url` | string | `https://api-service.api.prod.actual.ai` | Actual API endpoint |
| `runner` | string | `claude-cli` | AI backend: `claude-cli`, `anthropic-api`, `openai-api`, `codex-cli`, `cursor-cli` |
| `model` | string | `claude-sonnet-4-6` (Claude) / `gpt-5.2` (OpenAI) | Model for tailoring |
| `output_format` | string | `claude-md` | Output format: `claude-md`, `agents-md`, `cursor-rules` |
| `anthropic_api_key` | string | — | Fallback Anthropic key (if `ANTHROPIC_API_KEY` env var not set) |
| `openai_api_key` | string | — | Fallback OpenAI key (if `OPENAI_API_KEY` env var not set) |
| `cursor_api_key` | string | — | Fallback Cursor key (if `CURSOR_API_KEY` env var not set) |
| `max_budget_usd` | float | — | Maximum budget per tailoring invocation (USD) |
| `max_turns` | integer | 10 | Max agentic turns per invocation (claude-cli only) |
| `max_tokens` | integer | 16384 | Max output tokens (anthropic-api only) |
| `batch_size` | integer | 15 | Number of ADRs per tailoring batch |
| `concurrency` | integer | 10 | Max concurrent projects during tailoring |
| `invocation_timeout_secs` | integer | 600 | Per-project tailoring timeout |
| `include_categories` | list | — | ADR categories to always include |
| `exclude_categories` | list | — | ADR categories to always exclude |
| `include_general` | boolean | — | Whether to include language-agnostic ADRs |
| `max_per_framework` | integer | — | Max ADRs per framework |
| `update_check.enabled` | boolean | `true` | Daily update notice and `actual version --check` (see [UPDATE_CHECK.md](UPDATE_CHECK.md)) |
| `telemetry.enabled` | boolean | `true` | Set to `false` to stop every telemetry event (see [PRIVACY.md](../PRIVACY.md)) |
| `telemetry.otlp_logs_endpoint` | string | `/v1/logs` on `api_url` | OTLP/HTTP logs URL for ADR context, use, breach and override events, check events and session-end events, used as-is with no redirects followed: HTTPS, or plain HTTP to localhost only. Unset, release builds send to the default and debug builds send nothing; turning telemetry off stops both. When it is at the address `actual advisor` uses (`ACTUAL_API_URL` when it's set, otherwise `https://api-service.api.prod.actual.ai`) and you're signed in, requests carry your sign-in (see [PRIVACY.md](../PRIVACY.md)) |
| `telemetry.otlp_file` | string | — | Absolute path of a local file each ADR, check and session-end event request is appended to (see [PRIVACY.md](../PRIVACY.md)) |

### Example config

```yaml
runner: anthropic-api
model: claude-sonnet-4-6
output_format: claude-md
max_budget_usd: 0.50
telemetry:
  enabled: false
```

### Environment variable overrides

| Variable | Effect |
|----------|--------|
| `ACTUAL_CONFIG` | Exact path to the config file |
| `ACTUAL_CONFIG_DIR` | Directory containing `config.yaml` |
| `ANTHROPIC_API_KEY` | Anthropic API key (overrides config file) |
| `OPENAI_API_KEY` | OpenAI API key (overrides config file) |
| `CURSOR_API_KEY` | Cursor API key (overrides config file) |
| `ACTUAL_NO_UPDATE_CHECK` | Set to any value to turn off the update check |
| `ACTUAL_NO_TELEMETRY` | Set to any non-empty value for the same effect as `telemetry.enabled: false`, for CI and one-off runs |
| `ACTUAL_OTLP_LOGS_ENDPOINT` | OTLP logs URL for ADR context, use, breach and override events, check events and session-end events (overrides config file) |
| `ACTUAL_OTLP_FILE` | Local file for ADR context, use, breach and override events, check events and session-end events (overrides config file) |
