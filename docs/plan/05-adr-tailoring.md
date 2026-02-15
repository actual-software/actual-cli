# 05 - ADR Tailoring & Formatting (Combined)

## Overview

After fetching generic ADRs from the API, the CLI invokes Claude Code to **both tailor and format** them in a single step. This combined invocation transforms generic policies into repository-specific guidance and produces the rendered CLAUDE.md markdown content, including deciding which files to create (root and/or subdirectory CLAUDE.md files).

## Why Combined Tailoring + Formatting?

Combining tailoring and formatting into a single Claude Code invocation:

1. **Fewer API calls**: One invocation per project batch instead of separate tailor + format steps
2. **Better coherence**: Claude Code can organize the markdown naturally while tailoring, avoiding a disconnect between structured data and rendered output
3. **AI-generated formatting**: Matches sprintreview's approach where Claude Code decides how to structure the markdown content

## What Tailoring Does

1. **Path specificity**: Reference actual directories and files in the repo
2. **Convention alignment**: Match existing naming patterns, file structures, and code style
3. **Conflict resolution**: Identify where ADRs conflict with existing code and add migration notes
4. **Deduplication**: Merge overlapping ADRs from different framework matches
5. **Relevance filtering**: Drop ADRs that don't apply (e.g., a "use TypeORM migrations" ADR when the repo already uses Prisma)

## Pre-filtering

Before sending ADRs to Claude Code, the CLI pre-filters:

1. **Rejected ADRs**: Remove ADRs whose IDs appear in the per-repo rejection list (stored in `~/.actualai/actual/config.yaml`, keyed by SHA-256 of the git origin URL)
2. The filtered ADR set is what gets sent to Claude Code

## Invocation

Tailoring+formatting is done per project batch. Claude Code explores the repo and produces file output:

```bash
claude -p \
  --json-schema '<schema>' \
  --output-format json \
  --allow-dangerously-skip-permissions \
  --allowedTools "Read" "Glob(*)" "Grep(*)" \
  --tools "Read,Glob,Grep" \
  --model sonnet \
  --max-turns 15 \
  --no-session-persistence \
  "<tailoring + formatting prompt>"
```

Key flags:
- `--json-schema`: Enforces the outer JSON structure (files array, summary). The `content` field contains free-form markdown which the CLI validates separately.
- `--allow-dangerously-skip-permissions` + `--allowedTools`: Non-interactive read-only operation
- **No Bash access**: Tailoring is restricted to Read/Glob/Grep only (no Bash, unlike analysis)
- **No default budget cap**: The `--max-budget-usd` flag is available but not set by default. Users can configure it via `actual config set max_budget_usd 0.50` or pass `--max-budget-usd` on the command line.

## Prompt Design

The tailoring prompt includes:

1. **The generic ADRs** as structured input (JSON array)
2. **Repository context** from the analysis step (project paths, detected frameworks)
3. **Existing CLAUDE.md file paths** (the CLI tells Claude Code which paths already have CLAUDE.md files; Claude Code reads them if needed)
4. **Instructions** on how to tailor and format

### Prompt Template

```
You are tailoring Architecture Decision Records (ADRs) for a specific codebase
and generating CLAUDE.md file content.

## Repository Context
- Projects: {projects_json}
- Package Manager: {package_manager}

## Existing CLAUDE.md Files
The following CLAUDE.md files already exist in the repo (read them if you need to
understand the current state, but only generate content for the managed section):
{existing_claude_md_paths}

## ADRs to Tailor

{adr_json_array}

## Instructions

1. **Tailor each ADR**: Examine the repository to verify applicability. Reference
   actual paths, files, and patterns. Identify conflicts with existing code. Merge
   duplicates. Keep policies concise (1-2 sentences each).

2. **Skip inapplicable ADRs**: If an ADR doesn't apply (e.g., it recommends Tailwind
   but the project uses styled-components), mark it as skipped with a reason.

3. **Decide file layout**: Generate a root CLAUDE.md for general/cross-cutting rules.
   Generate subdirectory CLAUDE.md files (e.g., apps/web/CLAUDE.md) for projects
   that have project-specific ADRs. Only create subdirectory files when there are
   ADRs specific to that project -- don't create empty or redundant files.

4. **Format as markdown**: Generate the content that goes inside the managed section
   markers. Use terse, actionable directive-style instructions optimized for Claude's
   consumption. Organize content naturally -- use headings, bullet points, and inline
   code references as appropriate.

5. **Preserve intent**: Don't change the fundamental decision -- only make it more
   specific and actionable for this codebase.

Return your response as a JSON object matching the provided schema.
```

## Output Schema

```json
{
  "type": "object",
  "properties": {
    "files": {
      "type": "array",
      "items": {
        "type": "object",
        "properties": {
          "path": {
            "type": "string",
            "description": "File path relative to repo root (e.g., 'CLAUDE.md' or 'apps/web/CLAUDE.md')"
          },
          "content": {
            "type": "string",
            "description": "Rendered markdown content for inside the managed section markers"
          },
          "reasoning": {
            "type": "string",
            "description": "Brief explanation of what rules this file contains and why"
          },
          "adr_ids": {
            "type": "array",
            "items": { "type": "string" },
            "description": "UUIDs of the ADRs included in this file"
          }
        },
        "required": ["path", "content", "reasoning", "adr_ids"]
      }
    },
    "skipped_adrs": {
      "type": "array",
      "items": {
        "type": "object",
        "properties": {
          "id": { "type": "string" },
          "reason": { "type": "string" }
        },
        "required": ["id", "reason"]
      },
      "description": "ADRs that were not applicable to this repo"
    },
    "summary": {
      "type": "object",
      "properties": {
        "total_input": { "type": "integer" },
        "applicable": { "type": "integer" },
        "not_applicable": { "type": "integer" },
        "files_generated": { "type": "integer" }
      }
    }
  },
  "required": ["files", "skipped_adrs", "summary"]
}
```

The CLI validates the JSON structure via `--json-schema` and additionally checks:
- Each `content` field is non-empty markdown
- Each `path` field ends with `CLAUDE.md`
- `adr_ids` reference valid ADR IDs from the input set

## Batching Strategy

For large ADR sets (>20 total), batch them to avoid context window limits:

1. **Group by category**: ADRs in the same category are tailored together (they may reference each other)
2. **Batch size**: Default 15 ADRs per Claude Code invocation. Configurable via `actual config set batch_size 20` in `~/.actualai/actual/config.yaml`.
3. **Sequential batches**: Later batches include a summary of earlier results (which files were created, which ADRs were placed where) to maintain consistency and avoid duplication

```
Batch 1: ADRs in "Architecture & Structural Patterns" (10 ADRs)
Batch 2: ADRs in "Testing, Quality & Reliability" (8 ADRs) + context from Batch 1
Batch 3: ADRs in "Build, Delivery & Tooling" (5 ADRs) + context from Batches 1-2
```

When batching, the CLI merges the `files` arrays from each batch. If multiple batches produce content for the same file path, the CLI concatenates the content sections.

## Concurrency

For monorepos, tailoring runs projects in parallel with a configurable concurrency limit (default: 3). Configurable via `actual config set concurrency 5`.

```
Tailoring ADRs to your repository...
  ├── [1/5] apps/web (Next.js + React): tailoring 20 ADRs... done
  ├── [2/5] apps/api (Fastify): tailoring 15 ADRs... done
  ├── [3/5] packages/shared: tailoring 10 ADRs...
  ├── [4/5] packages/db: tailoring 8 ADRs... (waiting)
  └── [5/5] packages/auth: tailoring 6 ADRs... (waiting)
```

Note: Since the combined invocation now produces file output (including the root CLAUDE.md), the concurrency model may produce overlapping root file content. The CLI must merge root CLAUDE.md content from multiple parallel invocations.

## Cost Management

- **Model**: Sonnet by default (good enough for tailoring+formatting, much cheaper than Opus)
- **Budget cap**: Optional `--max-budget-usd` flag (no default). Users can set a per-project cap if desired.
- **Turn limit**: `--max-turns 15` to prevent infinite exploration loops
- **Read-only tools**: Only Read, Glob, Grep -- no Bash execution during tailoring
- **`--no-tailor` flag**: Users can skip tailoring entirely; raw ADRs from the bank are still formatted by a simpler Claude Code invocation (formatting only, no repo exploration)

Estimated costs per run:
- Small project (5-10 ADRs): ~$0.02-0.05 with Sonnet
- Medium project (15-25 ADRs): ~$0.05-0.15 with Sonnet
- Large monorepo (50+ ADRs across 5 projects): ~$0.20-0.50 with Sonnet

## Error Handling

| Scenario | Handling |
|----------|----------|
| Claude Code returns skipped ADRs | Show count to user during confirmation |
| Budget exceeded | Stop tailoring for that project, use untailored ADRs for remaining |
| Claude Code returns malformed JSON | Retry once; on second failure, error and exit |
| Timeout (>120s per project) | Kill subprocess, warn user, suggest `--verbose` |
| Empty content field | Warn, skip that file |
| Overlapping file paths from batches | Merge content sections for the same path |
