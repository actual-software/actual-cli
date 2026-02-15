# 03 - Repository Analysis

## Overview

Repository analysis is the first substantive step after environment validation. The goal is to identify:

1. **Project structure**: Is this a single project or a monorepo with sub-projects?
2. **Languages**: What programming languages are used in each project?
3. **Frameworks**: What major frameworks are used (Next.js, FastAPI, Rails, etc.)?

## Approach: Claude Code Subprocess

We invoke Claude Code in print mode with a structured output schema. Claude Code uses its built-in tools (Read, Glob, Grep) to explore the repository naturally.

### Invocation

```bash
claude -p \
  --json-schema '<schema>' \
  --output-format json \
  --allow-dangerously-skip-permissions \
  --allowedTools "Read" "Glob(*)" "Grep(*)" "Bash(git:*)" "Bash(ls:*)" \
  --tools "Read,Glob,Grep,Bash" \
  --model sonnet \
  --max-turns 10 \
  --no-session-persistence \
  "Analyze this repository and identify its structure, languages, and frameworks. <prompt>"
```

Key flags:
- `--json-schema`: Forces structured JSON output matching our expected shape
- `--output-format json`: Parseable response
- `--allow-dangerously-skip-permissions`: Enables the permission bypass option without activating it by default; combined with `--allowedTools` for non-interactive operation.
- `--allowedTools`: Explicit allowlist for read-only tools plus git and ls commands.
- `--tools "Read,Glob,Grep,Bash"`: Available tool set (Bash included for git/ls commands during analysis)
- `--model sonnet`: Use Sonnet for cost efficiency (repo analysis doesn't need Opus)
- `--max-turns 10`: Prevent runaway exploration
- `--no-session-persistence`: Don't save the session (it's ephemeral)

### JSON Schema for Output

```json
{
  "type": "object",
  "properties": {
    "is_monorepo": {
      "type": "boolean",
      "description": "Whether this repository contains multiple distinct projects"
    },
    "projects": {
      "type": "array",
      "items": {
        "type": "object",
        "properties": {
          "path": {
            "type": "string",
            "description": "Relative path from repo root (use '.' for root-level project)"
          },
          "name": {
            "type": "string",
            "description": "Human-readable project name"
          },
          "languages": {
            "type": "array",
            "items": {
              "type": "string",
              "enum": ["typescript", "javascript", "python", "rust", "go", "java", "kotlin", "swift", "ruby", "php", "c", "cpp", "csharp", "scala", "elixir", "other"]
            }
          },
          "frameworks": {
            "type": "array",
            "items": {
              "type": "object",
              "properties": {
                "name": {
                  "type": "string",
                  "description": "Framework name (e.g., 'nextjs', 'fastapi', 'rails')"
                },
                "category": {
                  "type": "string",
                  "enum": ["web-frontend", "web-backend", "mobile", "desktop", "cli", "library", "data", "ml", "devops", "testing"]
                }
              },
              "required": ["name", "category"]
            }
          },
          "package_manager": {
            "type": "string",
            "description": "Package manager used (npm, pnpm, yarn, cargo, pip, poetry, etc.)"
          },
          "description": {
            "type": "string",
            "description": "Brief description of what this project does"
          }
        },
        "required": ["path", "name", "languages", "frameworks"]
      }
    }
  },
  "required": ["is_monorepo", "projects"]
}
```

### Prompt Design

The analysis prompt should instruct Claude Code to:

1. Look at the root-level directory structure
2. Check for monorepo indicators:
   - Workspace files: `pnpm-workspace.yaml`, `lerna.json`, `nx.json`, `turbo.json`
   - Workspace fields in `package.json`
   - Multiple `Cargo.toml` files with a workspace `Cargo.toml`
   - `go.work` file
3. For each detected project, examine:
   - Package manifests (`package.json`, `Cargo.toml`, `pyproject.toml`, `go.mod`, `Gemfile`, etc.)
   - Framework-specific config files (`next.config.js`, `vite.config.ts`, `angular.json`, etc.)
   - Entry points and source directory structure
4. Classify languages and frameworks using the taxonomy enum values
5. Return the structured JSON

### Example Prompt

```
Analyze this repository's structure, programming languages, and frameworks.

Instructions:
- First, check the root directory for monorepo indicators (workspace files, lerna.json, nx.json, turbo.json, pnpm-workspace.yaml, go.work, Cargo workspace).
- If this is a monorepo, identify each sub-project by looking in common directories (apps/, packages/, services/, libs/, modules/).
- For each project (or the root if it's a single project), identify:
  - The primary programming language(s) by examining package manifests and source files
  - Major frameworks by looking at dependencies and configuration files
  - The package manager used
- Use the exact enum values specified in the JSON schema for languages, framework categories, etc.
- For framework names, use lowercase with hyphens (e.g., "nextjs", "fastapi", "ruby-on-rails", "express", "react", "vue", "angular", "django", "flask", "spring-boot", "gin", "actix-web").
- Be thorough but focus on major/primary frameworks, not every utility library.
```

## Response Parsing

The Rust CLI will:

1. Spawn `claude` as a subprocess
2. Capture stdout as JSON
3. Deserialize into a `RepoAnalysis` struct using `serde`
4. Validate the response (at least one project, valid enum values)
5. Present results to the user for confirmation

### Error Handling

| Scenario | Handling |
|----------|----------|
| Claude Code returns invalid JSON | Parse error, retry once with simplified prompt |
| Claude Code returns empty projects | Offer to retry with `--verbose`. No manual fallback in v1. |
| Claude Code times out | Timeout after 60s, suggest `--verbose` and retry |
| Claude Code exits non-zero | Capture stderr, display to user, suggest `--verbose` |

## User Confirmation

After analysis, the user confirms the detected projects:

```
Detected projects:
  1. apps/web       TypeScript [nextjs, react]
  2. apps/api       TypeScript [fastify]
  3. packages/shared TypeScript []

[a]ccept  [r]etry analysis  [q]uit
>
```

v1 does not support interactive editing of detected projects (adding, removing, or correcting). Users can either accept the result or retry the analysis (which re-invokes Claude Code). If analysis consistently fails, the user should check `--verbose` output for details.

## Performance Considerations

- Sonnet model for cost efficiency
- `--max-turns 10` to bound exploration time
- Read-only tools plus Bash for git/ls commands during analysis (no Bash for tailoring)
- Typical analysis should complete in 5-15 seconds
- **Caching**: Analysis results are cached in `~/.actualai/actual/config.yaml`, keyed to the git HEAD commit hash. If HEAD hasn't changed since the last analysis, the cached result is used. This is skipped if not in a git repo.
