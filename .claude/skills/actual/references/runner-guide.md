# Runner Guide Reference

Deep reference for all 5 runners. Load this when setting up a runner, troubleshooting runner-specific issues, or answering model compatibility questions.

## Table of Contents

- [Overview](#overview)
- [claude-cli](#claude-cli)
- [anthropic-api](#anthropic-api)
- [openai-api](#openai-api)
- [codex-cli](#codex-cli)
- [cursor-cli](#cursor-cli)
- [Runner Resolution Order](#runner-resolution-order)
- [Model-to-Runner Inference](#model-to-runner-inference)
- [Troubleshooting](#troubleshooting)

## Overview

Runners are the AI backends that power `actual sync`'s tailoring step. Each runner connects to a different AI provider or local CLI tool.

| Runner | Type | Binary | Auth Method | Default Model |
|--------|------|--------|-------------|---------------|
| claude-cli | CLI wrapper | `claude` | `claude auth login` | claude-sonnet-4-6 |
| anthropic-api | Direct API | (none) | `ANTHROPIC_API_KEY` env var | claude-sonnet-4-6 |
| openai-api | Direct API | (none) | `OPENAI_API_KEY` env var | gpt-5.2 |
| codex-cli | CLI wrapper | `codex` | `OPENAI_API_KEY` env var or `codex login` (ChatGPT OAuth) | gpt-5.2 |
| cursor-cli | CLI wrapper | `cursor-agent` | Optional `CURSOR_API_KEY` env var | `auto` |

## claude-cli

**The default runner.** Wraps the Claude Code CLI binary.

### Requirements

- `claude` binary in PATH
- Authenticated via `claude auth login`

### Setup

```bash
# Install Claude Code CLI (see Claude Code docs)
# Then authenticate:
claude auth login
```

### Authentication Check

```bash
actual auth
```

Returns auth status. If not authenticated, the error `ClaudeNotAuthenticated` (exit code 2) is returned during sync.

### Model Compatibility

- Default: `claude-sonnet-4-6`
- Supports short aliases: `sonnet`, `opus`, `haiku`
- Supports full claude model names (though those auto-infer to anthropic-api)

### Config

```bash
actual config set runner claude-cli
actual config set model claude-sonnet-4-6  # or just "sonnet"
```

## anthropic-api

Direct Anthropic API access without the Claude CLI binary.

### Requirements

- `ANTHROPIC_API_KEY` environment variable set

### Setup

```bash
export ANTHROPIC_API_KEY="sk-ant-..."
```

### Authentication Check

```bash
actual auth  # Shows API key status
```

If the key is missing, the error `ApiKeyMissing` (exit code 2) is returned.

### Model Compatibility

- Default: `claude-sonnet-4-6`
- Supports all `claude-*` model names
- Full model names (e.g., `claude-sonnet-4-6`) auto-infer to this runner

### Config

```bash
actual config set runner anthropic-api
actual config set model claude-sonnet-4-6
# Or set the API key in config (stored with 0600 permissions):
actual config set anthropic_api_key "sk-ant-..."
```

## openai-api

Direct OpenAI API access.

### Requirements

- `OPENAI_API_KEY` environment variable set

### Setup

```bash
export OPENAI_API_KEY="sk-..."
```

### Model Compatibility

- Default: `gpt-5.2`
- Supports `gpt-*`, `o1*`, `o3*`, `o4*`, `chatgpt-*` model names

### Config

```bash
actual config set runner openai-api
actual config set model gpt-5.2
# Or set the API key in config:
actual config set openai_api_key "sk-..."
```

## codex-cli

Wraps the Codex CLI binary. Supports two authentication modes.

### Requirements

- `codex` binary in PATH
- One of:
  - `OPENAI_API_KEY` environment variable, OR
  - ChatGPT OAuth via `codex login`

### Setup

```bash
# Install Codex CLI (see Codex docs)
# Then either:
export OPENAI_API_KEY="sk-..."
# Or use ChatGPT OAuth:
codex login
```

### ChatGPT OAuth Limitation

When using ChatGPT OAuth (via `codex login`), the codex-cli runner **only supports the default model**. If you specify an explicit model, you will get the `CodexCliModelRequiresApiKey` error (exit code 2).

To use a specific model with codex-cli, you must set `OPENAI_API_KEY` instead of using OAuth.

### Model Error Fallback

If codex-cli fails with a model-related error, the CLI automatically falls back to the openai-api runner and retries. This is transparent to the user.

### Model Compatibility

- Default: `gpt-5.2`
- Supports `codex-*`, `gpt-*-codex*` model names
- Also supports `gpt-*`, `o1*`, `o3*`, `o4*`, `chatgpt-*`

### Config

```bash
actual config set runner codex-cli
actual config set model gpt-5.2
```

## cursor-cli

Wraps the Cursor agent CLI binary.

### Requirements

- `agent` binary in PATH
- Optional: `CURSOR_API_KEY` environment variable

### Setup

```bash
# Install Cursor CLI (see Cursor docs)
# Optionally set API key:
export CURSOR_API_KEY="..."
```

### Model Compatibility

- Uses cursor's default model
- Config key `cursor_model` sets the model (auto-infers cursor-cli runner)

### Config

```bash
actual config set runner cursor-cli
actual config set cursor_model <model-name>
```

## Runner Resolution Order

When determining which runner to use, the CLI checks in this priority order:

1. **`--model` CLI flag** (if provided): infers runner from model name pattern
2. **`cursor_model` config key** (if set): always implies cursor-cli
3. **`model` config key** (if set): infers runner from model name pattern
4. **`--runner` CLI flag or `runner` config key**: explicit runner choice
5. **Default**: claude-cli

The `--runner` flag always takes precedence when explicitly set alongside `--model`. But when only `--model` is provided, the runner is inferred from the model name.

## Model-to-Runner Inference

The `infer_from_model` function maps model names to runners:

| Model Pattern | Regex / Match | Inferred Runner |
|---------------|---------------|-----------------|
| Short aliases: `sonnet`, `opus`, `haiku` | Exact match (case-insensitive) | claude-cli |
| Full Anthropic: `claude-*` | Starts with `claude-` | anthropic-api |
| OpenAI standard: `gpt-*`, `o1*`, `o3*`, `o4*`, `chatgpt-*` | Starts with prefix (non-codex) | codex-cli |
| Codex models: `codex-*`, `gpt-*-codex*` | Contains `codex` | codex-cli |
| Unrecognized | No match | Error with known model list |

### Edge Cases

- `chatgpt-*` goes to codex-cli (not a separate runner)
- Short aliases (`sonnet`) go to claude-cli, while full names (`claude-sonnet-4-6`) go to anthropic-api
- This means `actual sync --model sonnet` uses claude-cli but `actual sync --model claude-sonnet-4-6` uses anthropic-api

## Troubleshooting

### Runner Not Found

| Error | Missing Binary | Install |
|-------|---------------|---------|
| ClaudeNotFound | `claude` | Install Claude Code CLI |
| CodexNotFound | `codex` | Install Codex CLI |
| CursorNotFound | `agent` | Install Cursor CLI |

Check with: `which claude`, `which codex`, `which agent`

### Authentication Failures

| Error | Runner | Fix |
|-------|--------|-----|
| ClaudeNotAuthenticated | claude-cli | `claude auth login` |
| CodexNotAuthenticated | codex-cli | Set `OPENAI_API_KEY` or `codex login` |
| ApiKeyMissing | anthropic-api / openai-api | Set the required env var |
| CodexCliModelRequiresApiKey | codex-cli + OAuth | Set `OPENAI_API_KEY` (OAuth only supports default model) |

### Runner Timeout

If the runner exceeds `invocation_timeout_secs` (default 600s):

```bash
# Increase timeout
actual config set invocation_timeout_secs 1200
```

### Model Mismatch

If you get an error about an unrecognized model:

```bash
# List all known models
actual models

# Check what runner a model maps to
actual runners
```
