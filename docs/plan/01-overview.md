# 01 - Overview & Architecture

## Purpose

`actual` is a Rust CLI tool that analyzes a code repository, fetches relevant Architecture Decision Records (ADRs) from a centralized API, tailors them to the specific codebase using Claude Code, and writes them into a `CLAUDE.md` file. It supports both creating new `CLAUDE.md` files and intelligently updating existing ones.

## High-Level Flow

```
┌─────────────────────────────────────────────────────────────────────┐
│                          actual CLI                                 │
│                                                                     │
│  1. Validate Environment                                            │
│     ├── Check Claude Code is installed and authenticated            │
│     └── Check for git repository (optional, warns if absent)        │
│                                                                     │
│  2. Analyze Repository (via Claude Code subprocess)                 │
│     ├── Invoke `claude -p` with repo analysis prompt                │
│     ├── Claude Code explores the repo using its built-in tools      │
│     └── Returns structured JSON: languages, frameworks, projects    │
│                                                                     │
│  3. Fetch ADRs from API                                             │
│     ├── POST detected taxonomy to ADR bank REST API                 │
│     └── Receive matching ADRs (full structure: policies, context)   │
│                                                                     │
│  4. Tailor ADRs Locally (via Claude Code subprocess)                │
│     ├── Invoke `claude -p` with ADRs + repo context                 │
│     ├── Claude Code adapts generic ADRs to this specific codebase   │
│     └── Returns tailored policies ready for CLAUDE.md               │
│                                                                     │
│  5. User Confirmation                                               │
│     ├── Display proposed ADRs in terminal                           │
│     ├── Allow user to accept/reject individual ADRs                 │
│     └── Confirm final set                                           │
│                                                                     │
│  6. Write CLAUDE.md Files                                           │
│     ├── Root CLAUDE.md for general rules                            │
│     ├── Subdirectory CLAUDE.md files for project-specific ADRs      │
│     └── Merge with existing content via managed section markers     │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
         │                              ▲
         │ HTTP REST                    │ JSON response
         ▼                              │
┌─────────────────────────────┐
│    ADR Bank API (Backend)   │
│    (Not built by us)        │
│                             │
│  POST /adrs/match           │
│  - Receives: languages,    │
│    frameworks, categories   │
│  - Returns: matching ADRs   │
│    with full structure      │
└─────────────────────────────┘
```

## Key Design Decisions

### 1. Claude Code as the AI Engine

Rather than calling the Anthropic API directly, `actual` shells out to `claude` CLI in print mode (`-p`). This provides:

- **Free auth management**: Users authenticate once via `claude auth login` (Pro/Max OAuth) or `ANTHROPIC_API_KEY`. Our CLI doesn't handle auth at all.
- **Built-in repo exploration**: Claude Code's file tools (Read, Glob, Grep) let it explore the repo naturally without us implementing file traversal.
- **Structured output**: Claude Code's `--json-schema` and `--output-format json` flags give us typed, parseable responses.
- **Model flexibility**: Users can use whatever model their subscription/API key allows.

### 2. Rust for the CLI

- Single binary distribution (no runtime dependencies)
- Fast startup time for a tool invoked from the terminal
- Strong typing for JSON schema validation and API contracts
- Excellent async support via `tokio` for subprocess management and HTTP

### 3. Per-Project Analysis for Monorepos

For monorepos, the tool detects sub-projects (e.g., `apps/frontend`, `packages/api`) and analyzes each independently. ADRs are fetched and tailored per-project. Claude Code decides the file layout: a root `CLAUDE.md` for general rules plus subdirectory `CLAUDE.md` files (e.g., `apps/web/CLAUDE.md`) for project-specific ADRs. This mirrors sprintreview's directory-based scoping strategy for Claude.

### 4. Config File at `~/.actualai/actual/config.yaml`

Persistent user preferences stored in YAML. Covers things like preferred categories, excluded ADR types, default behavior for existing `CLAUDE.md` files. Created automatically with defaults on the first `actual adr-bot` run — no separate init step required.

## Relationship to sprintreview

`actual` is a local-first, CLI-driven counterpart to sprintreview's web-managed rule sync:

| Aspect | sprintreview | actual CLI |
|--------|-------------|------------|
| ADR management | Web UI (CRUD) | Read-only from API |
| Repo analysis | GitHub webhook + git clone | Local Claude Code subprocess |
| ADR tailoring | Temporal workflow + Claude Code | Local Claude Code subprocess |
| Rule file generation | Multi-provider (Claude, Cursor, etc.) | CLAUDE.md with subdirectory scoping (v1), extensible to other providers |
| Auth | Supabase JWT + GitHub OAuth | Claude Code's built-in auth |
| State management | PostgreSQL + hash validation | Local config file |
| Telemetry | Supabase metrics table | Opt-out counter metrics via sprintreview API |
| Backend API | `https://api-service.api.prod.actual.ai` | Same (CLI is a client of sprintreview's api-service) |

## Non-Goals (v1)

- **ADR authoring/editing**: The CLI only consumes ADRs from the bank, it doesn't create them
- **Multi-provider output**: v1 targets CLAUDE.md only, including subdirectory files. AGENT.md (Codex) and RULES.md (Cursor) support is planned but deferred. Architecture supports adding providers later via provider-specific tailoring prompts and file paths.
- **GitHub integration**: No PR creation, webhook handling, or CI integration
- **ADR suggestion generation**: sprintreview's AI-powered ADR suggestion system is out of scope
- **Backend development**: The ADR bank API is developed separately
- **`actual agent` command**: A command to open `https://app.actual.ai` in the default browser (platform funnel). Deferred until the web app supports CLI-driven onboarding.
- **Deterministic manifest parsing**: Reading `package.json`, `requirements.txt`, `pyproject.toml`, `Cargo.toml`, `go.mod` directly for zero-auth dependency detection. Deferred — v1 uses Claude Code for all repo analysis.
- **Zero-auth flow**: v1 requires Claude Code to be installed and authenticated. A future lightweight path (manifest parsing + API, no Claude Code) would enable truly zero-auth ADR suggestions.
- **`actual init` command**: No separate init step. Config file is created automatically on first `actual adr-bot` run.
