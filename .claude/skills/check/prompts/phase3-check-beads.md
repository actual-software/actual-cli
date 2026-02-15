# Phase 3: Check Bead Status (Batched)

You are a bead status checker for a bead sync check. Your job is to check the current status of a batch of beads and determine which ones should be closed.

## Inputs

- `BATCH_NUMBER`: {{BATCH_NUMBER}}
- `BEAD_IDS`: {{BEAD_IDS}} (JSON array of bead IDs to check, e.g. `["actual-cli-abc", "actual-cli-def"]`)

Read `.check/commits.json` to get the commit details for each bead.

## Tasks

For each bead ID in your batch:

1. Run `bd show <bead-id>` to get current status.
2. Determine action:
   - If status is `closed` → action: `skip`, reason: `already closed`
   - If status is `open` or `in_progress` and has commits in main → action: `close`
   - If `bd show` returns "not found" → action: `skip`, reason: `not found`
3. For beads to close, find the most recent commit hash from `.check/commits.json`.

## Output

Write the following JSON to `.check/bead-status/batch-{{BATCH_NUMBER}}.json`:

```json
{
  "batch": 1,
  "beads": [
    {
      "id": "actual-cli-abc",
      "current_status": "open",
      "title": "Fix generation timeout",
      "commits_in_main": ["abc1234"],
      "action": "close",
      "close_message": "Fixed in commit abc1234 - Fix generation timeout"
    },
    {
      "id": "actual-cli-def",
      "current_status": "closed",
      "title": "Add input validation",
      "commits_in_main": ["def5678"],
      "action": "skip",
      "reason": "already closed"
    }
  ]
}
```

## Rules

- Process ALL beads in your batch — don't stop on errors.
- If `bd show` fails for a bead, record action: `skip` with the error as reason.
- Do NOT run `bd close` — that's Phase 4's job.
- The `close_message` should reference the commit hash and include a brief summary.
