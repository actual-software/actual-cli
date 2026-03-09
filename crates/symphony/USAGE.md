# Symphony

Symphony is a long-running service that orchestrates coding agents to work on issues from Linear. It polls your Linear project for active issues, creates isolated workspaces for each one, and runs Claude Code sessions to complete the work — with automatic retries, multi-turn continuation, and a live web dashboard.

## Table of Contents

- [Quick Start](#quick-start)
- [Configuration Reference](#configuration-reference)
- [CLI](#cli)
- [HTTP Dashboard & API](#http-dashboard--api)
- [Prompt Template](#prompt-template)
- [Workspace Hooks](#workspace-hooks)
- [How It Works](#how-it-works)
- [Agent Environment Variables](#agent-environment-variables)
- [Logging](#logging)
- [Dynamic Reload](#dynamic-reload)
- [Trust Model](#trust-model)
- [Architecture](#architecture)

## Quick Start

### 1. Create a WORKFLOW.md

Create a `WORKFLOW.md` file in your working directory. This single file contains both the configuration (YAML front matter) and the prompt template (markdown body):

```markdown
---
tracker:
  kind: linear
  team_key: MY-TEAM
  api_key: $LINEAR_API_KEY
polling:
  interval_ms: 30000
workspace:
  root: /tmp/symphony_workspaces
agent:
  max_concurrent_agents: 5
  max_turns: 20
server:
  port: 7070
---

You are working on issue {{ issue.identifier }}: {{ issue.title }}.

{% if issue.description %}
## Issue Description

{{ issue.description }}
{% endif %}

{% if issue.labels.size > 0 %}
Labels: {% for label in issue.labels %}{{ label }}{% unless forloop.last %}, {% endunless %}{% endfor %}
{% endif %}

{% if attempt %}
This is retry attempt #{{ attempt }}. Review what was done previously and continue.
{% endif %}

## Instructions

1. Read the issue description carefully.
2. Explore the codebase to understand the relevant code.
3. Implement the fix or feature.
4. Run tests to verify your changes.
5. Commit your changes and create a pull request.
```

### 2. Set your Linear API key

Symphony automatically loads `.env.local` and `.env` files at startup (via `dotenvy`). You can also export the key directly:

```bash
# Option A: .env.local file (recommended, gitignored)
echo 'LINEAR_API_KEY=lin_api_xxxxxxxxxxxxx' >> .env.local

# Option B: Environment variable
export LINEAR_API_KEY=lin_api_xxxxxxxxxxxxx
```

### 3. Run Symphony

```bash
# Uses ./WORKFLOW.md by default, starts dashboard on port from config
symphony

# Explicit workflow path
symphony path/to/WORKFLOW.md

# Override the dashboard port (or start it even if not in config)
symphony --port 7070
```

Open `http://localhost:7070` to see the live dashboard.

## Configuration Reference

All configuration lives in the YAML front matter of `WORKFLOW.md`. Changes to this file are detected and applied automatically without restart (see [Dynamic Reload](#dynamic-reload)).

### tracker

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `kind` | string | (required) | Issue tracker type. Currently only `linear` |
| `endpoint` | string | `https://api.linear.app/graphql` | GraphQL endpoint |
| `api_key` | string | `$LINEAR_API_KEY` | API key. Supports `$VAR` for env var indirection |
| `team_key` | string | (required) | Linear team key (e.g. `ACTCLI`) |
| `active_states` | list or CSV | `Todo, In Progress` | Issue states that trigger agent dispatch |
| `terminal_states` | list or CSV | `Closed, Cancelled, Canceled, Duplicate, Done` | Issue states that stop work and clean up workspaces |

### polling

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `interval_ms` | integer | `30000` | Poll interval in milliseconds |

### workspace

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `root` | path | `<temp>/symphony_workspaces` | Root directory for per-issue workspaces. Supports `~` and `$VAR` |

### hooks

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `after_create` | shell script | (none) | Runs when a workspace is newly created. Failure aborts workspace creation |
| `before_run` | shell script | (none) | Runs before each agent attempt. Failure aborts the attempt |
| `after_run` | shell script | (none) | Runs after each agent attempt. Failure is logged and ignored |
| `before_remove` | shell script | (none) | Runs before workspace deletion. Failure is logged and ignored |
| `timeout_ms` | integer | `60000` | Timeout for all hooks |

### agent

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `max_concurrent_agents` | integer | `10` | Maximum parallel agent sessions |
| `max_turns` | integer | `20` | Maximum agent turns per worker run. Dynamically injected into the agent command (`--max-turns`) and environment (`SYMPHONY_MAX_TURNS`) |
| `max_retry_backoff_ms` | integer | `300000` (5m) | Cap on exponential retry backoff |
| `max_concurrent_agents_by_state` | map | `{}` | Per-state concurrency limits (e.g. `todo: 2, in progress: 3`) |

### coding_agent

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `command` | shell command | `claude -p --output-format stream-json --verbose --dangerously-skip-permissions --max-turns 20` | Command to launch the coding agent |
| `permission_mode` | string | `bypassPermissions` | Permission mode for agent sessions |
| `turn_timeout_ms` | integer | `3600000` (1h) | Timeout per agent turn |
| `stall_timeout_ms` | integer | `300000` (5m) | Kill agent if no events for this long. Set `0` to disable |

> **Deprecated**: The `codex` key is still accepted as a fallback but will be removed in a future release. Rename it to `coding_agent`.

### server

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `port` | integer | (none) | Port for the HTTP dashboard/API. If omitted, the server is not started. Use `0` for an ephemeral port |
| `bind` | string | `127.0.0.1` | IP address to bind the HTTP server to. Use `0.0.0.0` to listen on all interfaces |

## CLI

```
symphony [OPTIONS] [WORKFLOW_PATH]
```

| Argument / Flag | Type | Default | Description |
|-----------------|------|---------|-------------|
| `WORKFLOW_PATH` | positional | `WORKFLOW.md` | Path to the workflow file |
| `--port` | integer | (none) | Start HTTP dashboard on this port. Overrides `server.port` in WORKFLOW.md |
| `--bind` | string | `127.0.0.1` | Bind address for the HTTP dashboard. Overrides `server.bind` in WORKFLOW.md. Use `0.0.0.0` to listen on all interfaces |

## HTTP Dashboard & API

When `--port` or `server.port` is configured, Symphony starts an HTTP server on `{bind}:{port}` (default `127.0.0.1`) with a live web dashboard and JSON API. Use `--bind 0.0.0.0` or `server.bind: "0.0.0.0"` to listen on all interfaces.

### Dashboard (GET /)

The web dashboard is a server-rendered HTML page with a dark theme. It auto-refreshes and shows:

**Aggregate stats** at the top:
- **Total Tokens** — cumulative tokens used across all sessions (formatted as K/M for large numbers)
- **Input Tokens** — total prompt/input tokens
- **Output Tokens** — total completion/output tokens
- **Seconds Running** — total agent runtime

**Running Sessions** table with columns:

| Column | Description |
|--------|-------------|
| Identifier | Linear issue key (e.g. `ACTCLI-42`) |
| State | Current Linear state (e.g. `In Progress`) |
| Session | Claude session ID |
| Turns | Number of agent turns completed |
| Last Event | Most recent agent event type |
| Tokens | Total tokens for this session |
| Started | When this worker was dispatched |

**Retry Queue** table with columns:

| Column | Description |
|--------|-------------|
| Identifier | Linear issue key |
| Attempt | Retry attempt number |
| Due At (ms) | Timestamp when retry fires (epoch ms) |
| Error | Error message from the failed attempt |

**Rate Limits** section:
- Pretty-printed JSON of any rate limit data reported by the agent
- Shows "No rate limit data" if none available

### JSON API

All API responses use `Content-Type: application/json`. CORS is enabled for all origins.

#### GET /api/v1/state

Full snapshot of orchestrator state.

```json
{
  "generated_at": "2025-01-15T10:30:00Z",
  "counts": {
    "running": 2,
    "retrying": 1
  },
  "running": [
    {
      "issue_id": "abc123",
      "issue_identifier": "ACTCLI-42",
      "state": "In Progress",
      "session_id": "sess-abc",
      "turn_count": 3,
      "last_event": "turn_completed",
      "last_message": "Running cargo test...",
      "started_at": "2025-01-15T10:00:00Z",
      "last_event_at": "2025-01-15T10:28:00Z",
      "input_tokens": 15000,
      "output_tokens": 8000,
      "total_tokens": 23000
    }
  ],
  "retrying": [
    {
      "issue_id": "def456",
      "identifier": "ACTCLI-43",
      "attempt": 2,
      "due_at_ms": 1705312200000,
      "error": "turn timeout"
    }
  ],
  "codex_totals": {
    "input_tokens": 150000,
    "output_tokens": 80000,
    "total_tokens": 230000,
    "seconds_running": 3600.5
  },
  "rate_limits": null
}
```

#### GET /api/v1/:issue_identifier

Look up a specific issue by its Linear identifier (e.g. `GET /api/v1/ACTCLI-42`). Returns details if the issue is currently running or in the retry queue.

**Running issue:**
```json
{
  "issue_identifier": "ACTCLI-42",
  "status": "running",
  "issue_id": "abc123",
  "workspace_path": "/tmp/symphony_workspaces/ACTCLI-42",
  "session": {
    "session_id": "sess-abc",
    "turn_count": 3,
    "last_event": "turn_completed",
    "last_message": "Implementing feature...",
    "started_at": "2025-01-15T10:00:00Z",
    "last_event_at": "2025-01-15T10:28:00Z",
    "input_tokens": 15000,
    "output_tokens": 8000,
    "total_tokens": 23000
  },
  "retry": null
}
```

**Retrying issue:**
```json
{
  "issue_identifier": "ACTCLI-43",
  "status": "retrying",
  "issue_id": "def456",
  "workspace_path": "/tmp/symphony_workspaces/ACTCLI-43",
  "session": null,
  "retry": {
    "issue_id": "def456",
    "identifier": "ACTCLI-43",
    "attempt": 2,
    "due_at_ms": 1705312200000,
    "error": "turn timeout"
  }
}
```

**Not found (404):**
```json
{
  "error": {
    "code": "issue_not_found",
    "message": "No running or retrying issue with identifier 'ACTCLI-99'"
  }
}
```

#### POST /api/v1/refresh

Trigger an immediate poll cycle instead of waiting for the next interval.

Returns `202 Accepted`:
```json
{
  "queued": true,
  "requested_at": "2025-01-15T10:30:00Z"
}
```

If the message channel is full, the response includes `"coalesced": true` — the refresh will be coalesced with an already-pending poll.

#### Error Responses

Non-API routes return:
- `404` for unknown paths
- `405` for unsupported HTTP methods

```json
{
  "error": {
    "code": "not_found",
    "message": "Unknown route: /api/v2/foo"
  }
}
```

## Prompt Template

The markdown body after the YAML front matter is the prompt template, rendered per-issue using [Liquid](https://shopify.github.io/liquid/) syntax.

### Available Variables

**`issue`** (object):

| Variable | Type | Description |
|----------|------|-------------|
| `issue.id` | string | Internal tracker ID |
| `issue.identifier` | string | Human-readable key (e.g. `ACTCLI-42`) |
| `issue.title` | string | Issue title |
| `issue.description` | string or nil | Issue description body |
| `issue.priority` | integer or nil | Priority (lower = higher priority) |
| `issue.state` | string | Current tracker state name |
| `issue.branch_name` | string or nil | Tracker-provided branch name |
| `issue.url` | string or nil | Issue URL |
| `issue.labels` | array | Lowercase label strings |
| `issue.blocked_by` | array | Blocker objects with `id`, `identifier`, `state` |
| `issue.created_at` | string or nil | ISO-8601 creation timestamp |
| `issue.updated_at` | string or nil | ISO-8601 update timestamp |

**`attempt`** (integer or nil):
- `nil` on first run
- Integer on retry or continuation

### Template Examples

Conditional retry instructions:

```liquid
{% if attempt %}
This is retry attempt #{{ attempt }}. Check previous work and continue.
{% else %}
Start fresh on this issue.
{% endif %}
```

Iterating labels:

```liquid
{% for label in issue.labels %}
- {{ label }}
{% endfor %}
```

Checking blockers:

```liquid
{% if issue.blocked_by.size > 0 %}
Warning: This issue has {{ issue.blocked_by.size }} blockers.
{% endif %}
```

## Workspace Hooks

Hooks are shell scripts that run in the workspace directory at specific lifecycle points. They are useful for cloning repositories, installing dependencies, managing branches, and running cleanup.

### Hook Lifecycle

```
Issue dispatched
  |
  v
[workspace dir created] --> after_create (clone repo, install deps)
  |
  v
[agent turn starts]     --> before_run   (fetch latest, set up branch)
  |
  v
[agent runs...]
  |
  v
[agent turn ends]       --> after_run    (log results, collect artifacts)
  |
  v
[issue reaches terminal state]
  |
  v
[workspace removed]     --> before_remove (archive logs, clean up)
```

### Example: Full workspace setup

```yaml
hooks:
  after_create: |
    git clone --branch main git@github.com:my-org/my-repo.git .
    npm install
  before_run: |
    git fetch origin main
    git checkout main
    git pull
    # Create or reuse a feature branch
    BRANCH="symphony/$(basename $(pwd) | tr '[:upper:]' '[:lower:]')"
    git checkout "$BRANCH" 2>/dev/null || git checkout -b "$BRANCH" origin/main
  after_run: |
    echo "Run complete at $(date)" >> .symphony-log
  timeout_ms: 300000
```

### Hook Execution Rules

- Hooks run via `bash -lc <script>` in the workspace directory
- Hooks do **not** support Liquid template syntax — they are raw shell scripts
- The workspace directory name is the sanitized issue identifier (e.g. `ACTCLI-42`), accessible via `$(basename $(pwd))`
- All hooks are subject to the configured timeout (`hooks.timeout_ms`, default 60s)
- `after_create` and `before_run` failures are **fatal** (abort the operation)
- `after_run` and `before_remove` failures are **logged and ignored**

## How It Works

### Poll-Dispatch Loop

Every `polling.interval_ms`, Symphony:

1. **Reconciles** running issues — checks for stalls, refreshes tracker states
2. **Validates** the workflow configuration
3. **Fetches** candidate issues from Linear (via GraphQL with pagination)
4. **Sorts** by priority (ascending), then creation time (oldest first)
5. **Dispatches** eligible issues until concurrency slots are exhausted

### Issue Eligibility

An issue is dispatched only if:

- It has required fields (id, identifier, title, state)
- Its state is in `active_states` and not in `terminal_states`
- It's not already running, claimed, or recently completed
- Global concurrency limit (`max_concurrent_agents`) has available slots
- Per-state concurrency limit (if configured) has available slots
- If in `Todo` state: no non-terminal blockers exist

### Multi-Turn Sessions

Each worker can run multiple agent turns within a single session:

1. **First turn** uses the full rendered prompt template
2. **Subsequent turns** use a continuation prompt ("Continue working on issue X. This is turn N of M.")
3. After each turn, the worker checks if the issue is still in an active state on Linear
4. Turns continue up to `agent.max_turns`
5. If the issue transitions to a terminal state mid-session, the worker stops cleanly

### Retry Behavior

| Exit Reason | Behavior |
|-------------|----------|
| **Normal** (agent exits 0) | Schedules a continuation check after 1 second |
| **Failed** (non-zero exit) | Exponential backoff, capped at `max_retry_backoff_ms` |
| **Timed out** | Exponential backoff, capped at `max_retry_backoff_ms` |
| **Stalled** (no events) | Exponential backoff, capped at `max_retry_backoff_ms` |
| **Cancelled** (reconciliation) | No retry |

Backoff formula: `min(10000 * 2^(attempt-1), max_retry_backoff_ms)`

Example with default `max_retry_backoff_ms: 300000` (5m):

| Attempt | Delay |
|---------|-------|
| 1 | 10s |
| 2 | 20s |
| 3 | 40s |
| 4 | 80s |
| 5 | 160s |
| 6+ | 300s (capped) |

Retries continue indefinitely with backoff capped at `max_retry_backoff_ms` (§8.2 spec compliance).

### Reconciliation

Every poll tick, Symphony reconciles all running workers:

- **Stall detection**: If no agent events for `stall_timeout_ms`, the worker is killed and scheduled for retry
- **State refresh**: Fetches current states for all running issues from Linear
  - **Terminal state** (e.g. Merged, Closed): stops worker, cleans up workspace
  - **Still active**: updates the in-memory snapshot
  - **Neither active nor terminal** (e.g. moved to Backlog): stops worker without cleanup

### Startup Cleanup

On startup, Symphony queries Linear for issues in terminal states and removes any stale workspace directories left from prior runs.

## Agent Environment Variables

Symphony automatically injects the following environment variables into each agent subprocess:

| Variable | Source | Description |
|----------|--------|-------------|
| `LINEAR_API_KEY` | `tracker.api_key` | Linear API key for GraphQL requests |
| `LINEAR_TEAM_KEY` | `tracker.team_key` | Linear team key (e.g. `ACTCLI`) |

These are only set when the corresponding config value is non-empty. This allows agents to interact with Linear directly — for example, to create follow-up issues or update issue states.

### Example: Agent creates a follow-up issue

```bash
# Find the team ID
TEAM_ID=$(gh api -X POST https://api.linear.app/graphql \
  -H "Authorization: $LINEAR_API_KEY" \
  -f query='{ teams { nodes { id name } } }' \
  --jq '.data.teams.nodes[] | select(.name == "my-team") | .id')

# Create a follow-up issue
gh api -X POST https://api.linear.app/graphql \
  -H "Authorization: $LINEAR_API_KEY" \
  -f query="mutation {
    issueCreate(input: {
      teamId: \"$TEAM_ID\"
      title: \"Follow-up: fix edge case\"
      description: \"Discovered during work on parent issue\"
      priority: 3
    }) { issue { identifier url } }
  }"
```

## Logging

Symphony uses structured JSON logging to stderr via `tracing`. Control the log level with the `RUST_LOG` environment variable:

```bash
# Default (info level)
symphony

# Debug level for everything
RUST_LOG=debug symphony

# Symphony debug, other crates at warn
RUST_LOG=symphony=debug,warn symphony

# Trace-level for a specific module
RUST_LOG=symphony::orchestrator=trace,info symphony
```

Log entries include structured fields such as `issue_id`, `issue_identifier`, `attempt`, and `session_id` for filtering.

### Example: Filter logs for a specific issue

```bash
symphony 2>&1 | jq 'select(.fields.issue_identifier == "ACTCLI-42")'
```

## Dynamic Reload

Symphony watches `WORKFLOW.md` for changes using a file watcher. When the file is modified:

- Configuration is re-parsed and validated
- Prompt template is updated for future runs
- Poll interval, concurrency limits, and other settings take effect immediately
- In-flight agent sessions are **not** interrupted
- If the reload fails (e.g. invalid YAML), the last known good configuration is preserved and an error is logged

This means you can tune concurrency limits, polling intervals, or the prompt template without restarting Symphony.

## Trust Model

Symphony runs in high-trust mode by default:

- Claude Code runs with `--dangerously-skip-permissions` (auto-approves all tool use)
- Workspace isolation is the primary safety boundary — each issue gets its own directory
- Agent cwd is always set to the per-issue workspace path
- Workspace paths are validated to stay under the configured root
- `LINEAR_API_KEY` is passed to agent subprocesses for Linear API access

For stricter environments, customize `coding_agent.command` to remove `--dangerously-skip-permissions` and add appropriate permission controls.

## Architecture

```
WORKFLOW.md ──> [Workflow Loader] ──> [Config Layer] ──> [Orchestrator]
                                                              |
                    ┌─────────────────┬─────────────────┬─────┴─────┐
                    |                 |                 |           |
              [Linear Client]  [Workspace Mgr]  [Agent Runner]  [HTTP Server]
              (fetch issues)   (dirs + hooks)   (claude -p)    (dashboard)
                    |                 |                 |           |
                    v                 v                 v           v
              Linear API       /tmp/ws/PROJ-42    stream-json   :7070
```

### Module Overview

| Module | Purpose |
|--------|---------|
| `main.rs` | CLI entry point, signal handling, structured logging setup |
| `orchestrator.rs` | Core poll-dispatch-reconcile loop, worker lifecycle, retry state machine |
| `config.rs` | Typed config with defaults, env var resolution, validation |
| `model.rs` | Domain entities (Issue, LiveSession, OrchestratorState, AgentEvent, etc.) |
| `tracker.rs` | Linear GraphQL client with pagination and field normalization |
| `workspace.rs` | Workspace creation, reuse, hook execution, path safety |
| `prompt.rs` | Liquid template rendering with strict variable checking |
| `agent.rs` | Claude Code subprocess launch, event streaming, env var injection |
| `server.rs` | Axum HTTP server — dashboard UI and JSON API |
| `watcher.rs` | File watcher for dynamic WORKFLOW.md reload |
| `workflow.rs` | Parse WORKFLOW.md (YAML front matter + prompt body) |
| `error.rs` | Typed error taxonomy (25+ variants) |
