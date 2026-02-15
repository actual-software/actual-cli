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
3. Filter completed epics from ready work:
   - For each item in `bd ready` output
   - If item is an epic (`issue_type == "epic"`), check if all children are closed:
     ```bash
     bd show <epic-id> --children --json
     ```
   - Count total children and closed children
   - If `total_children > 0` AND `closed_children == total_children`, exclude from ready work list
   - Keep track of these completable epics separately for reporting
4. For the top 5 highest-priority ready items (after filtering), get details:
   ```bash
   bd show <id>
   ```
5. Estimate effort for each ready item: trivial / small / medium / large.
6. Formulate a recommendation on what to start with and why.

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
  "completable_epics": [
    {
      "id": "actual-cli-xyz",
      "title": "Epic: Some completed feature",
      "total_children": 5,
      "closed_children": 5,
      "note": "All children closed, epic can be manually closed"
    }
  ],
  "recommendation": {
    "start_with": "actual-cli-eco",
    "reason": "P0 priority, small effort, unblocks CI automation pipeline."
  }
}
```

## Rules

- Filter out completed epics: Epics where all children are closed should NOT appear in ready_work
- Report completable epics separately in the completable_epics array
- Order ready work by priority: P0 > P1 > P2 > P3
- Within the same priority, prefer smaller effort items (quicker wins)
- The recommendation should consider both priority and effort — a quick P0 fix beats a large P1
- Include a brief description summary (1-2 sentences) for each item
- Do NOT claim or update any beads — just report
- If an epic has no children (empty array from `bd show --children`), include it in ready_work (not filtered)
