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

### Plan-governance events (anonymous)

When `actual plan-check` or `actual impl-check` (including each command's
`--claude-hook` path Claude Code drives automatically) reaches a verdict --
i.e. it actually judged the plan or diff against at least one selected rule
-- and on `actual check-override`, the CLI sends discrete events describing
the governance outcome, proxied through `api-service.api.prod.actual.ai` to
PostHog. The CLI never talks to PostHog directly and holds no PostHog
credentials — the proxy is its only outbound path for this data.

A run that fails open before reaching a verdict -- no rules apply, no runner
is available, or the check itself fails -- sends no plan-governance events at
all. This stream cannot be used to measure how often `plan-check`/`impl-check`
run in total, or how often either fails open versus produces a verdict; it
only describes verdicts that were actually reached.

| Event | Sent when |
|-------|-----------|
| `plan_governance_check_started` | Built alongside `plan_governance_check_completed` and sent in the same batch, once a run has reached a verdict -- not emitted separately at the actual start of the check |
| `plan_governance_check_completed` | A `plan-check` or `impl-check` run reaches a verdict, or `check-override` records an override (see below -- overrides reuse this same event, distinguished only by `command`) |
| `plan_governance_rule_violation` | One rule did not conform (once per violating rule) |

Each event includes a subset of these properties:

| Property | Description |
|----------|--------------|
| `cli_version` | CLI version string |
| `command` | Which subcommand/mode ran (e.g. `"plan-check"`, `"plan-check --claude-hook"`, `"impl-check"`, `"impl-check --claude-hook"`, `"check-override"`) |
| `rule_id` | The internal id of the specific rule involved -- an Actual-internal identifier, same category as `adr_ids` above, not a file path |
| `rule_source` | The rule document's internal slug, derived from its filename -- not a filesystem path |
| `decision` | `allow`, `warn`, or `block` -- the outcome for this rule or run |
| `duration_ms` | How long the check took |
| `exit_code` | The process exit code |
| `repo_hash` | Same one-way SHA-256 hash as the sync counters above |
| `repo_url_hash` | Same one-way SHA-256 hash as the sync counters above |

**No plan text, diff content, matched rule file paths, or conflicting
plan/diff spans are ever sent** -- only the rule's internal id/slug and a
coarse allow/warn/block verdict.

`actual check-override` reports a cleared rule by sending
`plan_governance_check_completed` with `command` set to
`"check-override"` rather than a distinct event type -- it is not a
real check's completion, and it carries no `duration_ms`. A query over
`plan_governance_check_completed` that counts "checks run" or averages
`duration_ms` must filter `command != "check-override"`, or it will mix
override records into those aggregates.

Each event also carries a `distinct_id`: a random, opaque identifier
generated once on first use and stored locally
(`~/.actualai/actual/telemetry-id`), never derived from a username, email,
hostname, or any other identifying material. It exists so events from the
same installation can be grouped over time. It is generated independently of
`repo_hash`/`repo_url_hash` (i.e. not cryptographically derived from them),
but **every plan-governance event carries `distinct_id` and
`repo_hash`/`repo_url_hash` together in the same payload** -- that pairing is
how events are grouped per installation, and it also means an installation
can be correlated with the set of repositories it has run `plan-check` or
`impl-check` against over time. It does not, on its own, identify a person.

Each event also carries an `insert_id`: a fresh random UUID generated per
event, used only so the proxy can drop an accidental duplicate delivery of the
same event. It is not stored, not reused across events, and carries no
information about the installation, repository, or user.

All three telemetry opt-outs described below disable plan-governance events
identically -- they share the exact same opt-out check as the sync counters,
and are checked before any repo-identity hashing or `distinct_id` creation,
not just before the network send: an opted-out run never touches git and
never writes `~/.actualai/actual/telemetry-id`.

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
| `api-service.api.prod.actual.ai` | Telemetry counters, project metadata, plan-governance events | HTTPS (enforced) |
| PostHog (via the `api-service.api.prod.actual.ai` proxy only) | Plan-governance events, forwarded server-side | N/A -- the CLI never contacts PostHog directly |
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
