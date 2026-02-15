---
name: check
description: Comprehensive bead sync with main - reviews all commits, closes completed beads, reports ready work
compatibility: Requires bd (beads) CLI for issue tracking. Works with Claude Code, OpenCode, and other Agent Skills-compatible tools.
disable-model-invocation: true
metadata:
  author: actualai
  version: "3.0"
  workflow: beads-issue-tracking
---

# Check v3.0

Multi-agent bead sync with file-based checkpointing. Syncs beads with main branch, closes completed work, and reports available tasks.

## Architecture

This skill uses **sub-agent decomposition** to stay within context limits. Instead of one agent scanning all commits and checking all beads in sequence, it batches the work across multiple agents with file-based state in `.check/`.

## How to Run

```
/check
```

## When to Use

- **Start of work session**: Get comprehensive view of available work
- **After merging PRs**: Sync beads with completed work
- **"What should I work on next?"**: Prioritized ready-work list
- **Regular check-ins**: Ensure bead tracker is fully synced

## Orchestrator Workflow

You are the orchestrator. Execute these phases in order. Each phase spawns sub-agents using the Task tool with prompts from the `prompts/` directory alongside this file.

### Resume Check

Before starting, check if a previous run was interrupted:

```bash
cat .check/checkpoint.json 2>/dev/null
```

If `checkpoint.json` exists, skip any phase where `status` is `"done"`. For partially-completed phases (e.g. some batch files exist in `bead-status/`), only re-run missing batches.

### Init

```bash
mkdir -p .check/bead-status
```

Write `.check/checkpoint.json`:
```json
{"started_at": "<timestamp>", "phases": {}}
```

### Phase 1: Pull Latest

Spawn **1 general agent** with the prompt from `prompts/phase1-pull.md`.

- Agent pulls main, fetches beads-sync, syncs bead state.
- Agent writes `.check/pull-status.json`.
- Update checkpoint: `phases.phase1.status = "done"`.

### Phase 2: Scan Commits

Spawn **1 explore agent** with the prompt from `prompts/phase2-scan-commits.md`.

- Agent scans `git log` for the last 3 months and extracts bead IDs.
- Agent writes `.check/commits.json`.
- Update checkpoint: `phases.phase2.status = "done"`.

### Phase 3: Check Bead Status (parallel, batched)

Read `.check/commits.json` to get the list of unique bead IDs.

Split the bead IDs into batches of **10**. Spawn **one general agent per batch**, each with the prompt from `prompts/phase3-check-beads.md`.

Template replacements per agent:
- `{{BATCH_NUMBER}}` = 1, 2, 3, ...
- `{{BEAD_IDS}}` = JSON array of bead IDs for this batch

Each agent writes `.check/bead-status/batch-<N>.json`.

**Launch all batch agents in parallel.** Cap at 5 concurrent.

After all complete, update checkpoint: `phases.phase3.status = "done"`.

### Phase 4: Close Beads & Sync

Spawn **1 general agent** with the prompt from `prompts/phase4-close.md`.

- Agent reads all batch files, closes beads that need closing, runs `bd sync`.
- Agent writes `.check/close-results.json`.
- Update checkpoint: `phases.phase4.status = "done"`.

### Phase 5: Report Ready Work

Spawn **1 general agent** with the prompt from `prompts/phase5-report.md`.

- Agent runs `bd list`, `bd ready`, `bd show` for top items.
- Agent writes `.check/report.json`.
- Update checkpoint: `phases.phase5.status = "done"`.

### Final Output

Read `.check/report.json` and present the final report to the user:

```markdown
## Sync Complete

- **Commits reviewed**: <count> from last 3 months
- **Beads closed**: <count>
- **Sync status**: Pushed to beads-sync branch

### In Progress (<count>)

<list of in-progress items>

### Available Work (<count>)

#### P0 (<count>)
1. **<id>** - <title> (effort: <effort>)

#### P1 (<count>)
2. **<id>** - <title> (effort: <effort>)

#### P2 (<count>)
...

### Recommendation

Start with **<id>** (<title>) — <reason>.
```

### Cleanup

On **success** (all phases done): delete `.check/` directory.

On **failure** (any phase errored): keep `.check/` for debugging. Tell the user:
```
Check scratch directory preserved at .check/ for debugging.
Re-run /check to resume from the last checkpoint.
```

## Context Budget

| Agent | Instruction payload | Data payload | Total |
|-------|-------------------|--------------|-------|
| Phase 1 (pull) | ~1KB | ~1KB | ~2KB |
| Phase 2 (scan) | ~2KB | ~10KB git log | ~12KB |
| Phase 3 (per batch) | ~2KB | ~5KB bd show x10 | ~7KB |
| Phase 4 (close) | ~1KB | ~5KB batch results | ~6KB |
| Phase 5 (report) | ~2KB | ~10KB bd ready/show | ~12KB |

No single agent exceeds ~12KB. The monolithic v2.0 could exceed 100KB+ with large commit histories and many beads.

## Related

- [AGENTS.md](../../../AGENTS.md) - Beads workflow and sync documentation
- [Audit Skill](../audit/SKILL.md) - For finding new work via codebase audit
