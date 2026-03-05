# Symphony

Symphony is a long-running service that orchestrates coding agents to work on issues from Linear. It polls your Linear project for active issues, creates isolated workspaces for each one, and runs Claude Code sessions to complete the work.

## Quick Start

### 1. Create a WORKFLOW.md

Create a `WORKFLOW.md` file in your working directory:

```markdown
---
tracker:
  kind: linear
  project_slug: my-project
  api_key: $LINEAR_API_KEY
polling:
  interval_ms: 30000
workspace:
  root: /tmp/symphony_workspaces
agent:
  max_concurrent_agents: 5
  max_turns: 20
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

```bash
export LINEAR_API_KEY=lin_api_xxxxxxxxxxxxx
```

### 3. Run Symphony

```bash
symphony
```

Or with an explicit workflow path:

```bash
symphony path/to/WORKFLOW.md
```

## Configuration Reference

All configuration lives in the YAML front matter of `WORKFLOW.md`. Changes to this file are detected and applied automatically without restart.

### tracker

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `kind` | string | (required) | Issue tracker type. Currently only `linear` |
| `endpoint` | string | `https://api.linear.app/graphql` | GraphQL endpoint |
| `api_key` | string | `$LINEAR_API_KEY` | API key. Supports `$VAR` for env var indirection |
| `project_slug` | string | (required) | Linear project slug ID |
| `active_states` | list or CSV | `Todo, In Progress` | States that trigger dispatch |
| `terminal_states` | list or CSV | `Closed, Cancelled, Canceled, Duplicate, Done` | States that stop work and clean up |

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
| `max_turns` | integer | `20` | Maximum agent turns per worker run |
| `max_retry_backoff_ms` | integer | `300000` (5m) | Cap on exponential retry backoff |
| `max_concurrent_agents_by_state` | map | `{}` | Per-state concurrency limits (e.g., `todo: 2`) |

### codex

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `command` | shell command | `claude -p --output-format stream-json --verbose --dangerously-skip-permissions --max-turns 20` | Command to launch the coding agent |
| `permission_mode` | string | `bypassPermissions` | Permission mode for agent sessions |
| `turn_timeout_ms` | integer | `3600000` (1h) | Timeout per agent turn |
| `read_timeout_ms` | integer | `5000` | Request/response timeout during startup |
| `stall_timeout_ms` | integer | `300000` (5m) | Kill agent if no activity for this long. Set `0` to disable |

## Prompt Template

The markdown body after the YAML front matter is the prompt template, rendered per-issue using [Liquid](https://shopify.github.io/liquid/) syntax.

### Available Variables

**`issue`** (object):
- `issue.id` - Internal tracker ID
- `issue.identifier` - Human-readable key (e.g., `PROJ-42`)
- `issue.title` - Issue title
- `issue.description` - Issue description (may be nil)
- `issue.priority` - Priority number (lower = higher priority, may be nil)
- `issue.state` - Current tracker state name
- `issue.branch_name` - Tracker-provided branch name (may be nil)
- `issue.url` - Issue URL (may be nil)
- `issue.labels` - Array of lowercase label strings
- `issue.blocked_by` - Array of blocker objects with `id`, `identifier`, `state`
- `issue.created_at` - ISO-8601 creation timestamp (may be nil)
- `issue.updated_at` - ISO-8601 update timestamp (may be nil)

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

Hooks are shell scripts that run in the workspace directory at specific lifecycle points. They are useful for setting up repositories, installing dependencies, or running cleanup.

### Example: Clone a repo on workspace creation

```yaml
hooks:
  after_create: |
    git clone https://github.com/my-org/my-repo.git .
    npm install
  before_run: |
    git fetch origin main
    git checkout main
    git pull
  after_run: |
    echo "Run complete at $(date)" >> .symphony-log
```

### Hook Execution

- Hooks run via `bash -lc <script>` in the workspace directory
- All hooks are subject to the configured timeout (`hooks.timeout_ms`, default 60s)
- `after_create` and `before_run` failures are fatal (abort the operation)
- `after_run` and `before_remove` failures are logged and ignored

## How It Works

### Poll-Dispatch Loop

1. Every `polling.interval_ms`, Symphony:
   - **Reconciles** running issues (checks for stalls, refreshes tracker states)
   - **Validates** the workflow configuration
   - **Fetches** candidate issues from Linear
   - **Sorts** by priority (ascending), then creation time (oldest first)
   - **Dispatches** eligible issues until concurrency slots are exhausted

### Issue Eligibility

An issue is dispatched only if:
- It has required fields (id, identifier, title, state)
- Its state is in `active_states` and not in `terminal_states`
- It's not already running or claimed
- Global and per-state concurrency slots are available
- If in `Todo` state: no non-terminal blockers exist

### Multi-Turn Sessions

Each worker can run multiple agent turns within a single session:
1. First turn uses the full rendered prompt template
2. Subsequent turns use a continuation prompt
3. After each turn, the worker checks if the issue is still active
4. Turns continue up to `agent.max_turns`

### Retry Behavior

- **Normal completion**: Schedules a continuation check after 1 second
- **Failure/timeout/stall**: Exponential backoff starting at 10s, doubling each attempt, capped at `max_retry_backoff_ms`
- Formula: `min(10000 * 2^(attempt-1), max_retry_backoff_ms)`

### Reconciliation

Every poll tick:
- **Stall detection**: If no agent events for `stall_timeout_ms`, the worker is killed and retried
- **State refresh**: Fetches current states for all running issues from Linear
  - Terminal state: stops worker, cleans workspace
  - Still active: updates the in-memory snapshot
  - Neither active nor terminal: stops worker without cleanup

### Startup Cleanup

On startup, Symphony queries Linear for issues in terminal states and removes any stale workspace directories left from prior runs.

## Logging

Symphony uses structured JSON logging to stderr. Control the log level with the `RUST_LOG` environment variable:

```bash
# Default (info level)
symphony

# Debug level
RUST_LOG=debug symphony

# Only symphony debug, other crates at warn
RUST_LOG=symphony=debug,warn symphony
```

Log entries include `issue_id`, `issue_identifier`, and `session_id` fields for filtering.

## Dynamic Reload

Symphony watches `WORKFLOW.md` for changes. When the file is modified:
- Configuration is re-parsed and re-applied
- Prompt template is updated for future runs
- Poll interval, concurrency limits, and other settings take effect immediately
- In-flight agent sessions are not interrupted
- If the reload fails, the last known good configuration is preserved

## Trust Model

This implementation runs in high-trust mode:
- Claude Code runs with `--dangerously-skip-permissions` (auto-approves all tool use)
- Workspace isolation is the primary safety boundary
- Agent cwd is always the per-issue workspace path
- Workspace paths are validated to stay under the configured root

For stricter environments, customize `codex.command` to remove `--dangerously-skip-permissions` and add appropriate permission controls.

## Architecture

```
WORKFLOW.md
    |
    v
[Workflow Loader] --> [Config Layer] --> [Orchestrator]
                                              |
                        +---------------------+---------------------+
                        |                     |                     |
                   [Linear Client]     [Workspace Manager]   [Agent Runner]
                   (fetch issues)      (create/cleanup dirs) (launch claude)
                        |                     |                     |
                        v                     v                     v
                   Linear API          /tmp/symphony_ws/       claude -p ...
                                       PROJ-42/
```

### Module Overview

| Module | Purpose |
|--------|---------|
| `workflow.rs` | Parse WORKFLOW.md (YAML front matter + prompt body) |
| `config.rs` | Typed config with defaults, env var resolution, validation |
| `model.rs` | Domain entities (Issue, LiveSession, OrchestratorState, etc.) |
| `tracker.rs` | Linear GraphQL client with pagination and normalization |
| `workspace.rs` | Workspace creation, reuse, hooks, path safety |
| `prompt.rs` | Liquid template rendering with strict variable checking |
| `agent.rs` | Claude Code subprocess launch and event streaming |
| `orchestrator.rs` | Poll loop, dispatch, reconciliation, retry/backoff state machine |
| `watcher.rs` | File watcher for dynamic WORKFLOW.md reload |
| `error.rs` | Typed error taxonomy (25+ variants) |
| `main.rs` | CLI entry point with structured logging |
