---
name: audit
description: Comprehensive codebase audit that scans for bugs, dead code, half-implemented features, error handling gaps, cut corners, bad tests, and refactoring opportunities. Files detailed Linear issues organized under parent issues.
compatibility: Requires gh CLI with Linear API access for issue tracking. Works with Claude Code, OpenCode, and other Agent Skills-compatible tools.
argument-hint: "[focus-area] e.g. 'error handling' or 'src/generation/' or empty for full audit"
disable-model-invocation: true
metadata:
  author: actualai
  version: "4.0"
---

# Codebase Audit v4.0

Multi-agent, context-resilient codebase audit with file-based checkpointing.

## Architecture

This skill uses **sub-agent decomposition** to stay within context limits. Instead of one monolithic agent reading the entire codebase, it dispatches small, focused agents in parallel — each handling one directory, one grep category, or one epic. All intermediate state is written to `.audit/` as JSON files, so no single agent needs to hold accumulated state in context.

## How to Run

**Full audit:**
```
/audit
```

**Focused audit:**
```
/audit error handling
/audit src/generation/
```

## Orchestrator Workflow

You are the orchestrator. Execute these phases in order. Each phase spawns sub-agents using the Task tool with prompts from the `prompts/` directory alongside this file.

### Resume Check

Before starting, check if a previous run was interrupted:

```bash
cat .audit/checkpoint.json 2>/dev/null
```

If `checkpoint.json` exists, skip any phase where `status` is `"done"`. For partially-completed phases, check which sub-agent output files exist and only re-run the missing ones.

### Init

```bash
mkdir -p .audit/phase2 .audit/phase3 .audit/phase5
```

Write `.audit/checkpoint.json`:
```json
{"started_at": "<timestamp>", "focus": "<focus or full>", "phases": {}}
```

### Phase 1: Preparation

Spawn **1 general agent** with the prompt from `prompts/phase1-prep.md`.

- Replace `{{FOCUS}}` with the user's focus argument (or empty string).
- Agent writes `.audit/prep.json`.
- Update checkpoint: `phases.phase1.status = "done"`.

### Phase 2: Source Reading (parallel)

Read `.audit/prep.json` to get the directory list. Spawn **one explore agent per directory**, each with the prompt from `prompts/phase2-read.md`.

Split `src/` subdirectories that are large into separate agents for source and tests:
- Large directories with `FILE_FILTER` = files NOT matching `*_test.rs` or in `tests/` -> output `<dir>-src.json`
- Test files with `FILE_FILTER` = `*_test.rs` or `tests/**/*.rs` -> output `<dir>-tests.json`

For smaller directories, use `FILE_FILTER` = `*.rs` (all files).

Template replacements per agent:
- `{{DIRECTORY}}` = the directory path
- `{{FILE_FILTER}}` = the file filter
- `{{OUTPUT_NAME}}` = slug for the output filename (e.g. `src-generation`, `src-cli`)

Each agent writes `.audit/phase2/<output-name>.json`.

**Launch all Phase 2 agents in parallel** (use multiple Task tool calls in one message).

Cap at 8 concurrent agents. If more than 8 directories, batch them.

After all complete, update checkpoint: `phases.phase2.status = "done"`.

### Phase 3: Automated Scans (parallel)

Spawn **one explore agent per scan category**, each with the prompt from `prompts/phase3-scan.md`.

Categories (7 agents):
1. `suppressed-errors`
2. `todo-markers`
3. `skipped-tests`
4. `unsafe-usage`
5. `hardcoded-values`
6. `unwrap-usage`
7. `bad-test-patterns`

Template replacement: `{{SCAN_CATEGORY}}` = the category name.

Each agent writes `.audit/phase3/<category>.json`.

**Launch all 7 agents in parallel.**

After all complete, update checkpoint: `phases.phase3.status = "done"`.

### Phase 4: Analysis & Merge

Spawn **1 general agent** with the prompt from `prompts/phase4-analyze.md`.

- Agent reads all `.audit/phase2/*.json` and `.audit/phase3/*.json`.
- Agent writes `.audit/phase4-findings.json`.
- Update checkpoint: `phases.phase4.status = "done"`.

### Phase 5: File Issues (parallel)

Read `.audit/phase4-findings.json` to get the list of epics with findings.

Spawn **one general agent per epic** that has findings, each with the prompt from `prompts/phase5-file-issues.md`.

Template replacement: `{{EPIC_KEY}}` = the epic name, `{{EPIC_SLUG}}` = slugified name (e.g. `bug-fixes`).

Each agent writes `.audit/phase5/<epic-slug>.json`.

**Launch all Phase 5 agents in parallel.**

After all complete, update checkpoint: `phases.phase5.status = "done"`.

### Phase 6: Verification

Spawn **1 general agent** with the prompt from `prompts/phase6-verify.md`.

- Agent reads all phase5 outputs and runs verification commands.
- Agent writes `.audit/phase6-verification.json`.
- If verification fails, report issues to the user and ask whether to proceed or fix.
- Update checkpoint: `phases.phase6.status = "done"`.

### Phase 7: Report

Spawn **1 general agent** with the prompt from `prompts/phase7-report.md`.

- Agent generates `.audit/report.json`.
- Update checkpoint: `phases.phase7.status = "done"`.

### Final Output

Read `.audit/report.json` and present the final report to the user:

```markdown
## Audit Complete

**Scope**: <scope from report>
**Files scanned**: <count>
**Findings**: <count> issues across <count> parent issues

### Findings by Category

1. **<category-name>** (<parent-issue-id>) - <count> findings
   - <finding-1-title> (P<N>)
   - <finding-2-title> (P<N>)

### Priority Distribution

- **P0**: <count> critical
- **P1**: <count> high
- **P2**: <count> medium
- **P3**: <count> cleanup

### Verification

- [x] All issues organized under parent issues
- [x] No duplicates found
- [x] Issues created in Linear (ACTCLI team)
```

### Cleanup

On **success** (all phases done, verification passed): delete `.audit/` directory.

On **failure** (any phase errored or verification failed): keep `.audit/` for debugging. Tell the user:
```
Audit scratch directory preserved at .audit/ for debugging.
Re-run /audit to resume from the last checkpoint.
```

## Context Budget

Each sub-agent stays well within context limits:

| Agent | Instruction payload | Data payload | Total |
|-------|-------------------|--------------|-------|
| Phase 2 (per dir) | ~3KB prompt | ~10-60KB source | ~65KB max |
| Phase 3 (per scan) | ~2KB prompt | ~5KB grep output | ~7KB |
| Phase 4 (merge) | ~3KB prompt | ~50KB findings | ~53KB |
| Phase 5 (per epic) | ~3KB prompt | ~20KB findings+cmds | ~23KB |
| Phase 6 (verify) | ~2KB prompt | ~10KB results | ~12KB |
| Phase 7 (report) | ~2KB prompt | ~15KB report data | ~17KB |

No single agent exceeds ~65KB. The monolithic v2.0 approach consumed ~1.1MB+.

## Related

- [AGENTS.md](../../../AGENTS.md) - Development workflow and issue tracking
- [WORKFLOW.md](../../../WORKFLOW.md) - Symphony configuration for autonomous orchestration
