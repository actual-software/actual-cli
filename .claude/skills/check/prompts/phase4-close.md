# Phase 4: Close Beads & Sync

You are the closer agent for a bead sync check. Your job is to close beads that have been completed (landed in main) and sync the bead state.

## Inputs

Read ALL files from `.check/bead-status/batch-*.json`.

## Tasks

1. Collect all beads where `action == "close"` across all batch files.
2. For each bead to close:
   ```bash
   bd close <bead-id> -m "<close_message>"
   ```
3. Close empty placeholder epics:
   a. Get all open epics:
      ```bash
      bd list --status open --type epic --json
      ```
   b. For each epic, safely check if it has children:
      ```bash
      # Get children count with fallback for missing field
      CHILDREN_COUNT=$(bd show <epic-id> --children --json | jq '.children | length // -1')
      ```
   c. Check if the epic is long-lived by examining its title and description for keywords:
      ```bash
      EPIC_INFO=$(bd show <epic-id> --json)
      DESCRIPTION=$(echo "$EPIC_INFO" | jq -r '.description // ""')
      TITLE=$(echo "$EPIC_INFO" | jq -r '.title // ""')
      IS_LONG_LIVED=false
      if echo "$DESCRIPTION $TITLE" | grep -qiE '(long.?lived|bug.?tracker|ongoing|maintenance|permanent)'; then
        IS_LONG_LIVED=true
      fi
      ```
   d. Only close the epic if ALL conditions are met:
      - `CHILDREN_COUNT == 0` (no children)
      - `IS_LONG_LIVED == false` (not a long-lived epic)
   e. Close qualifying epics:
      ```bash
      bd close <epic-id> -m "Closing empty placeholder epic with no child beads"
      ```
   f. Skip if:
      - `CHILDREN_COUNT == -1` (unexpected JSON structure)
      - `IS_LONG_LIVED == true` (long-lived epic must stay open per AGENTS.md)
      - Epic has children
4. After closing all beads and empty epics, sync to beads-sync branch:
   ```bash
   bd sync --full
   ```

## Output

Write the following JSON to `.check/close-results.json`:

```json
{
  "beads_closed": [
    {"id": "actual-cli-abc", "success": true},
    {"id": "actual-cli-ghi", "success": true}
  ],
  "beads_failed": [],
  "total_closed": 2,
  "empty_epics_closed": [
    {"id": "actual-cli-xyz", "success": true}
  ],
  "empty_epics_failed": [],
  "empty_epics_skipped": [
    {"id": "actual-cli-long-lived", "reason": "long-lived epic"},
    {"id": "actual-cli-abc", "reason": "has children"},
    {"id": "actual-cli-def", "reason": "failed to parse children"}
  ],
  "total_empty_epics_closed": 1,
  "total_empty_epics_skipped": 3,
  "sync": {
    "success": true,
    "error": null
  }
}
```

## Rules

- If `bd close` fails for a bead, record it in `beads_failed` with the error message. Continue with the next bead.
- If `bd close` fails for an empty epic, record it in `empty_epics_failed` with the error message. Continue with the next epic.
- If there are 0 beads to close, skip to empty epic closing and write `total_closed: 0`.
- If there are 0 empty epics to close, write `total_empty_epics_closed: 0`.
- Empty **placeholder** epics are automatically closed to prevent orphaned epics that were never used.
- **CRITICAL**: Long-lived epics (bug trackers, maintenance epics) MUST stay open even with zero children, per AGENTS.md rules. Check epic description for keywords before closing.
- Only close epics that are both empty AND not marked as long-lived.
- If `bd sync --full` fails, record the error but don't retry.
