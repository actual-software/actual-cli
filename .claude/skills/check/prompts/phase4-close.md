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
3. Close empty epics:
   a. Get all open epics:
      ```bash
      bd list --status open --type epic --json
      ```
   b. For each epic, check if it has children:
      ```bash
      bd show <epic-id> --children --json
      ```
   c. If `children.length == 0`, close the epic:
      ```bash
      bd close <epic-id> -m "Closing empty epic with no child beads"
      ```
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
  "total_empty_epics_closed": 1,
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
- Empty epics are automatically closed to prevent orphaned epics that were either never used or had work done outside the bead system.
- If `bd sync --full` fails, record the error but don't retry.
