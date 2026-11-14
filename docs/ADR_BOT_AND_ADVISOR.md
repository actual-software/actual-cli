# adr-bot and the Advisor

This page is the reference for `actual adr-bot` and `actual advisor`, which still ship with the CLI. For checking your agent's plans and changes against your rules, start with the [README](../README.md).

## adr-bot: ADR-backed context

Ask your agent to "set up Actual for this repository." The skill runs `actual adr-bot`, which analyzes your repository, fetches the ADRs that apply to it, tailors them to your codebase, and writes the result into the file your agent reads. Your agent starts every session already knowing how your team builds things. Content sits between managed markers, so later runs update it without touching anything you wrote yourself. Ask to "preview the ADR guidance" first and the skill runs it with `--dry-run`.

| Format | Flag | Output file |
|--------|------|-------------|
| Claude Code (default) | `--output-format claude-md` | `CLAUDE.md` |
| Agents | `--output-format agents-md` | `AGENTS.md` |
| Cursor Rules | `--output-format cursor-rules` | `.cursor/rules/actual-policies.mdc` |

<details>
<summary><strong>Direct CLI</strong></summary>

```bash
actual adr-bot --dry-run                       # preview without writing
actual adr-bot                                 # write CLAUDE.md
actual adr-bot --output-format agents-md       # write AGENTS.md instead
actual status                                  # check managed markers and staleness
```

</details>

### Running it

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

### Output formats

By default, `actual adr-bot` writes to `CLAUDE.md`. Content is wrapped in managed markers so future runs update cleanly without touching anything you've written yourself.

| Format | Flag | Output file |
|--------|------|-------------|
| Claude Code (default) | `--output-format claude-md` | `CLAUDE.md` |
| Agents | `--output-format agents-md` | `AGENTS.md` |
| Cursor Rules | `--output-format cursor-rules` | `.cursor/rules/actual-policies.mdc` |

### Flags

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

## The Advisor: org-scoped architecture answers

When your agent hits an architecture question, the skill asks the Advisor through `actual advisor`. The answer is grounded in your team's ADRs and cites its sources, so you can trace every recommendation back to a decision your team made. The question is scoped to the repository you're working in: the CLI matches the working tree's `origin` remote to a connected repository and falls back to your organization when nothing matches.

<details>
<summary><strong>Direct CLI</strong></summary>

```bash
actual advisor "How should we handle database access in a new service?"
actual advisor --repo actual-cli "..."       # scope to a repo by name
actual advisor --repo owner/actual-cli "..." # disambiguate a shared name
actual advisor --repo none "..."             # ask at the organization level
actual advisor --show-scope                  # print the active scope and exit
```

The scope you choose is remembered per repository; `--repo auto` returns to auto-detection.

</details>

### Ask the Advisor

`actual advisor` asks the Advisor an architecture question about your organization's codebase and prints the answer, along with any related ADRs, to your terminal. Unlike `adr-bot`, this command talks to your Actual AI account rather than a local AI runner, so sign in first with `actual login`.

```bash
actual login
actual advisor "When should we introduce a message queue?"
```

The command starts an Advisor job and polls until the answer is ready (up to a few minutes), writing progress to stderr and the final answer to stdout.

#### Scoping the question to a repository

By default the question is scoped to the repository you're standing in. `actual` reads the working tree's `origin` remote and, if one of your organization's connected repositories matches, scopes the answer to that repo. If nothing matches (or you aren't inside a git repo), the question runs at the organization level.

Override the scope with `--repo`, which accepts a repository **name**, an `owner/name` form (to disambiguate a name shared across owners), or a repo **UUID**:

```bash
actual advisor --repo actual-cli "..."        # by name
actual advisor --repo owner/actual-cli "..."  # by owner/name
actual advisor --repo 3f2a1c9e-... "..."      # by UUID
```

Two values are reserved keywords rather than repository names:

| Value | Effect |
|-------|--------|
| `--repo none` | Pin the scope to the organization level (opt out of repo scoping) |
| `--repo auto` | Forget any pin and return to git-remote auto-detection |

The scope you choose is **remembered per repository** (keyed by the working tree's `origin` remote) and reused on later `actual advisor` calls from the same tree, so you don't have to repeat `--repo` each time. Inspect the active scope without asking a question:

```bash
actual advisor --show-scope
```

To scope to a repository literally named `none` or `auto`, pass its `owner/name` form or UUID instead of the bare keyword.

#### Advisor flags

| Flag | What it does |
|------|-------------|
| `<QUERY>` | The architecture question to ask. Optional when the command is used only to change or show scope (`--show-scope`, or a bare `--repo` value with no question) |
| `--repo <REPO>` | Scope to a connected repository by name, `owner/name`, or UUID; `none` pins org level, `auto` resets to auto-detection. Omit to auto-detect from the `origin` remote |
| `--show-scope` | Print the remembered scope for the current repository and exit (reads local config only, no query) |
| `--org <ORG_ID>` | Organization to scope to (defaults to your signed-in org). Passing `--org` without `--repo` runs a one-shot organization-level query without changing the remembered scope |
| `--api-url <URL>` | Advisor API base URL (defaults to production; also settable via the `ACTUAL_API_URL` environment variable) |

### The `actual advisor` command

Ask the Advisor an org-scoped architecture question, grounded in your team's ADRs. Requires a prior actual login. It shows a `thinking…` indicator while it works, then prints the answer followed by the related ADRs it drew on, each with a confidence score. Queries run against the production api-service by default.

```bash
actual advisor "<question>"
```

Examples:

```bash
# Ask an org-scoped architecture question
actual advisor "Which router do we use for new Next.js pages?"

# Scope the question to one connected repo
actual advisor "What is our API error-handling convention?" --repo <REPO_UUID>
```

**Example output**, for `actual advisor "How should authentication and authorization be handled?"` (trimmed to the first matched ADR):

```text
advisor thinking…
## Relevant ADRs (5 found)

### ADR: Standardize External API Client Initialization with Explicit Credential Management for Applications

**Policy:** Applications must implement explicit credential management at external service boundaries with dependency injection of authenticated clients.

**Instructions:** Create clients in designated initialization modules or script entry points with explicit credential management through centralized secrets systems. Pass client instances through function signatures using dependency injection rather than creating clients inline or using global singletons…

**Scope:** In scope: All Python scripts and modules that interact with external HTTP APIs, integration with third-party services (Slack, GitHub, Claude, etc.), authentication and authorization flows for external services… Out of scope: Internal service-to-service communication, database connection management, file system access…

**Confidence:** High

_(4 more related ADRs returned)_
```

## Requirements

- For the Advisor: an Actual AI account (sign up at [actual.ai/cli](https://actual.ai/cli), or ask your agent to sign you in) and your repository onboarded at [app.actual.ai](https://app.actual.ai).
- For ADR-backed context and governance: a configured runner. Claude Code CLI is the default; `actual runners` lists the rest. ADR-backed context needs no account or API key.
