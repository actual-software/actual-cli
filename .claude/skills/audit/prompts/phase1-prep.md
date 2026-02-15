# Phase 1: Preparation

You are a preparation agent for a codebase audit. Your job is to gather project context and existing issue state so that subsequent audit agents have the information they need.

## Inputs

- `FOCUS`: {{FOCUS}} (empty string means full audit)

## Tasks

1. Read `AGENTS.md` to understand the project workflow, bd CLI, and conventions.
2. Run `bd list` to get all existing beads. Record open issue IDs and titles.
3. Run `bd list | grep epic` to get all existing epics with their IDs.
4. List all Rust source directories: `find . -name '*.rs' -not -path './.worktrees/*' -not -path './target/*' | sed 's|/[^/]*$||' | sort -u`
5. Count files per directory to understand codebase shape.
6. Identify: language (Rust), framework (clap CLI), architecture patterns, config approach.

## Output

Write the following JSON to `.audit/prep.json`:

```json
{
  "focus": "<focus area or 'full'>",
  "project": {
    "language": "rust",
    "framework": "<e.g. clap>",
    "patterns": ["<DI>", "<traits>", "..."]
  },
  "directories": [
    {"path": "src/cli/", "file_count": 8, "line_count": 2500},
    {"path": "src/generation/", "file_count": 5, "line_count": 1800}
  ],
  "existing_epics": [
    {"id": "actual-cli-xxx", "title": "Epic Title", "status": "open", "child_count": 5}
  ],
  "existing_beads": [
    {"id": "actual-cli-yyy", "title": "Bead Title", "status": "open", "parent": "actual-cli-xxx"}
  ]
}
```

## Rules

- Do NOT create any beads or epics — just gather information.
- Do NOT read source files — that's Phase 2's job.
- If FOCUS is set, still gather full project context but note the focus for downstream agents.
- Keep the output concise — titles and IDs only, no full descriptions.
