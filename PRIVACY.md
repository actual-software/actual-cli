# Privacy & Telemetry

Actual CLI collects minimal, anonymous telemetry to help us understand usage
patterns and improve the product. This document describes exactly what data
is collected, where it goes, and how to opt out.

## What data is collected

### Telemetry counters (anonymous)

On each successful run of `actual adr-bot`, the CLI sends counter metrics:

| Metric | Description |
|--------|-------------|
| `cli.sync.adrs_fetched` | Number of ADRs returned by the API |
| `cli.sync.adrs_tailored` | Number of ADRs successfully tailored by the LLM |
| `cli.sync.adrs_rejected` | Number of ADRs filtered out |
| `cli.sync.adrs_written` | Number of ADRs written to output files |
| `cli.sync.adrs_recommended` | Number of ADRs matched and recommended for this repo (only emitted when > 0) |

Each metric includes these tags:

| Tag | Description |
|-----|-------------|
| `repo_hash` | SHA-256 hash of `(repo_url + commit_hash)` -- the raw URL and commit are **never** sent |
| `repo_url_hash` | SHA-256 hash of the normalized repo URL alone -- stable across commits, used to count unique repos |
| `source` | Always `"actual-cli"` |
| `version` | CLI version string (e.g. `"0.1.2"`) |

The `cli.sync.adrs_recommended` metric also includes:

| Tag | Description |
|-----|-------------|
| `adr_ids` | Comma-separated list of ADR IDs that were matched for this repo (e.g. `"adr-001,adr-002"`) |

ADR IDs are internal Actual AI identifiers and are not sensitive. Repository identity
remains one-way hashed -- raw URLs are never sent.

**No personally identifiable information is collected.** No usernames, email
addresses, IP addresses, file contents, file paths, or source code are
included in telemetry.

### Project metadata (sent to the Actual API)

When fetching ADRs, the CLI sends a match request containing:

| Field | Description |
|-------|-------------|
| `path` | Relative path of each detected project (e.g. `"."`, `"packages/api"`) |
| `name` | Project directory name |
| `languages` | Detected programming languages (e.g. `["typescript", "python"]`) |
| `frameworks` | Detected frameworks with categories (e.g. `[{"name": "nextjs", "category": "web-framework"}]`) |

This metadata is used to match relevant ADRs to your project. **No source
code, file contents, or repository URLs are sent to the Actual API.**

### LLM tailoring (sent to your configured AI provider)

During the tailoring step, ADR content and a bundled summary of your
repository structure are sent to your configured AI provider (Anthropic,
OpenAI, or a local runner). This data is sent directly to the provider's
API -- it does not pass through Actual servers. Refer to your AI provider's
privacy policy for how they handle this data:

- [Anthropic Usage Policy](https://www.anthropic.com/policies)
- [OpenAI Privacy Policy](https://openai.com/policies/privacy-policy)

## Where data is sent

| Destination | Data | Protocol |
|-------------|------|----------|
| `api-service.api.prod.actual.ai` | Telemetry counters, project metadata | HTTPS (enforced) |
| Your AI provider (Anthropic/OpenAI) | ADR content + repo context for tailoring | HTTPS |

All non-localhost API communication enforces HTTPS. The CLI will reject
non-HTTPS API URLs.

## How to opt out of telemetry

There are three independent ways to disable telemetry:

### 1. Environment variable (recommended)

```bash
export ACTUAL_NO_TELEMETRY=1
```

Any non-empty value disables telemetry. Add this to your shell profile for
a permanent opt-out.

### 2. Config file

Add to `~/.actualai/actual/config.yaml`:

```yaml
telemetry:
  enabled: false
```

### 3. Compile-time

Build from source without the telemetry feature:

```bash
cargo build --release --no-default-features
```

## Data retention

Telemetry counters are aggregated and do not contain identifiers that could
be traced back to individual users. Both `repo_hash` and `repo_url_hash` are
one-way SHA-256 hashes -- the original repository URL cannot be recovered from
either of them.

## Changes to this policy

Material changes to data collection will be documented in this file and
noted in release changelogs.
