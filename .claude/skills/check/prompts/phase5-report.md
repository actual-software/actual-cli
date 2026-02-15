# Phase 5: Report Ready Work

You are the reporting agent for a bead sync check. Your job is to assess the current bead state and produce a prioritized work report.

## Inputs

Read:
- `.check/commits.json` — commit scan results
- `.check/close-results.json` — close operation results

## Tasks

1. Get current in-progress items:
   ```bash
   bd list --status in_progress
   ```
2. Get ready work:
   ```bash
   bd ready
   ```
3. For the top 5 highest-priority ready items, get details:
   ```bash
   bd show <id>
   ```
4. Estimate effort for each ready item: trivial / small / medium / large.
5. Formulate a recommendation on what to start with and why.

## Output

Write the following JSON to `.check/report.json`:

```json
{
  "sync_summary": {
    "commits_reviewed": 226,
    "beads_closed": 2,
    "beads_already_closed": 10,
    "beads_not_found": 1,
    "sync_success": true
  },
  "in_progress": [
    {"id": "actual-cli-foo", "title": "...", "assignee": "..."}
  ],
  "ready_work": [
    {
      "id": "actual-cli-eco",
      "title": "Fix CI auto-remediation workflow conditions",
      "priority": "P0",
      "effort": "small",
      "parent_epic": "actual-cli-drj",
      "description_summary": "CI workflow if conditions are wrong, causing auto-remediation to not trigger."
    }
  ],
  "recommendation": {
    "start_with": "actual-cli-eco",
    "reason": "P0 priority, small effort, unblocks CI automation pipeline."
  }
}
```

## Rules

- Order ready work by priority: P0 > P1 > P2 > P3.
- Within the same priority, prefer smaller effort items (quicker wins).
- The recommendation should consider both priority and effort — a quick P0 fix beats a large P1.
- Include a brief description summary (1-2 sentences) for each item.
- Do NOT claim or update any beads — just report.
