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
3. After closing all beads, sync to beads-sync branch:
   ```bash
   bd sync
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
  "sync": {
    "success": true,
    "error": null
  }
}
```

## Rules

- If `bd close` fails for a bead, record it in `beads_failed` with the error message. Continue with the next bead.
- If there are 0 beads to close, skip to sync and write `total_closed: 0`.
- If `bd sync` fails, record the error but don't retry.
