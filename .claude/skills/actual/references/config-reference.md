# Config Reference

All configuration keys, defaults, validation rules, and dotpath syntax. Load this when configuring the actual CLI.

## Table of Contents

- [Config File Location](#config-file-location)
- [All Config Keys](#all-config-keys)
- [Key Details](#key-details)
- [Dotpath Syntax](#dotpath-syntax)
- [Environment Variable Overrides](#environment-variable-overrides)
- [File Permissions](#file-permissions)

## Config File Location

**Default**: `~/.actualai/actual/config.yaml`

Override with environment variables:

| Env Var | Purpose | Notes |
|---------|---------|-------|
| `ACTUAL_CONFIG` | Exact path to config file | Takes precedence over everything |
| `ACTUAL_CONFIG_DIR` | Directory containing config | Must be absolute path, no `..` components |

To find the active config path:
```bash
actual config path
```

## All Config Keys

| Key | Type | Default | Validation | Purpose |
|-----|------|---------|------------|---------|
| `api_url` | string | (built-in) | URL format | API server URL |
| `model` | string | claude-sonnet-4-6 | non-empty | Model for Anthropic runners |
| `openai_model` | string | gpt-5.2 | non-empty | Model for OpenAI runners |
| `cursor_model` | string | (none) | non-empty | Model for cursor-cli (setting this auto-infers cursor-cli) |
| `runner` | enum | claude-cli | One of: claude-cli, anthropic-api, openai-api, codex-cli, cursor-cli | AI runner backend. `claude-cli` is never written to config (see note) |
| `batch_size` | u32 | 15 | min 1 | ADRs per API batch |
| `concurrency` | u32 | 10 | min 1 | Parallel API requests |
| `invocation_timeout_secs` | u64 | 600 | min 1 | Runner timeout in seconds |
| `max_budget_usd` | f64 | (none) | positive, finite | Spending cap for tailoring |
| `max_turns` | u32 | (none) | min 1 | Max conversation turns (claude-cli only) |
| `max_per_framework` | u32 | (none) | min 1 | Max ADRs per framework |
| `include_general` | bool | (none) | true/false | Include general (non-framework) ADRs |
| `include_categories` | string | (none) | comma-separated | Only include these ADR categories |
| `exclude_categories` | string | (none) | comma-separated | Exclude these ADR categories |
| `output_format` | enum | claude-md | One of: claude-md, agents-md, cursor-rules | Output file format |
| `anthropic_api_key` | string | (none) | non-empty | Anthropic API key |
| `openai_api_key` | string | (none) | non-empty | OpenAI API key |
| `telemetry.enabled` | bool | (none) | true/false | Enable/disable telemetry |

## Key Details

### Runner and Model Keys

Setting `model` directly is used for Anthropic runners (claude-cli, anthropic-api). Setting `openai_model` is used for OpenAI runners (openai-api, codex-cli). Setting `cursor_model` always implies cursor-cli.

The runner resolution order is:
1. `--model` CLI flag (infers runner from model name)
2. `openai_model` config (implies OpenAI runner)
3. `cursor_model` config (implies cursor-cli)
4. `model` config (infers runner from model name)
5. `runner` config or `--runner` flag
6. Default: claude-cli

**Important: `claude-cli` is never persisted to `config.yaml`.** It is the default runner, and storing it in the config would suppress model-based runner inference (e.g. setting an OpenAI model would be ignored if `runner: claude-cli` were present in the file). Running `actual config set runner claude-cli` prints a warning and clears the runner field rather than writing it. Existing config files that contain `runner: claude-cli` are silently normalized (the field is cleared) when the config is loaded.

### Numeric Keys

All numeric keys have a minimum of 1. Setting a value below the minimum results in a `ConfigError`.

- `batch_size`: Controls how many ADRs are fetched per API call. Higher values mean fewer round trips but larger payloads.
- `concurrency`: Controls how many API requests run in parallel. Higher values are faster but may hit rate limits.
- `invocation_timeout_secs`: How long to wait for the AI runner to respond per invocation. Increase this for slow models or large codebases.
- `max_budget_usd`: Must be positive and finite. Prevents runaway spending during tailoring.
- `max_turns`: Only used by claude-cli runner. Limits the number of conversation turns.

### Category Filtering

`include_categories` and `exclude_categories` accept comma-separated category names:

```bash
actual config set include_categories "security,performance"
actual config set exclude_categories "testing"
```

These are mutually exclusive in practice: use one or the other to filter which ADR categories are synced.

### API Keys in Config

API keys set via `config set` are stored in the config YAML file. The file is created with 0600 permissions (owner read/write only) on Unix systems.

```bash
actual config set anthropic_api_key "sk-ant-..."
actual config set openai_api_key "sk-..."
```

Environment variables (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`) take precedence over config file values.

## Dotpath Syntax

Config keys use dotpath notation for nested values:

```bash
# Top-level key
actual config set model claude-sonnet-4-6

# Nested key (uses dot separator)
actual config set telemetry.enabled false
```

### Get vs Set

```bash
# Show all config
actual config show

# Set a value
actual config set <key> <value>

# Show config file path
actual config path
```

There is no `config get <key>` subcommand. Use `config show` to view all values.

## Environment Variable Overrides

| Env Var | Overrides Config Key | Notes |
|---------|---------------------|-------|
| `ANTHROPIC_API_KEY` | `anthropic_api_key` | Takes precedence over config |
| `OPENAI_API_KEY` | `openai_api_key` | Takes precedence over config |
| `CURSOR_API_KEY` | (cursor auth) | Used by cursor-cli runner |
| `ACTUAL_CONFIG` | Config file path | Exact file path |
| `ACTUAL_CONFIG_DIR` | Config directory | Must be absolute, no `..` |

## File Permissions

On Unix systems, the config file is created with mode 0600 (owner read/write only). This protects API keys stored in the file.

If the config file exists with different permissions, the CLI does not change them. But new files are always created with 0600.
