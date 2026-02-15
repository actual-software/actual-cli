# Phase 2: Scan Commits

You are the commit scanner for a bead sync check. Your job is to scan recent commits on main and extract all bead IDs referenced in commit messages.

## Tasks

1. Get all commits from the last 3 months:
   ```bash
   git log --oneline main --since="3 months ago"
   ```
2. Extract every unique bead ID from commit messages. Bead IDs follow the pattern: `actual-cli-XXX` (where XXX is 3 alphanumeric characters).
3. For each unique bead ID, record which commit(s) reference it.

## Output

Write the following JSON to `.check/commits.json`:

```json
{
  "commits_scanned": 226,
  "date_range": {"from": "2025-11-14", "to": "2026-02-14"},
  "bead_ids_found": [
    {
      "bead_id": "actual-cli-abc",
      "commits": [
        {"hash": "abc1234", "message": "actual-cli-abc: Fix generation timeout"}
      ]
    },
    {
      "bead_id": "actual-cli-def",
      "commits": [
        {"hash": "def5678", "message": "actual-cli-def: Add input validation"}
      ]
    }
  ],
  "total_unique_beads": 15
}
```

## Rules

- Only extract IDs matching the `actual-cli-[a-z0-9]{3}` pattern.
- Deduplicate: if multiple commits reference the same bead, group them under one entry.
- Include the full commit message (one-line) for context.
- Do NOT run `bd show` — that's Phase 3's job.
- If `git log` returns no output, write `commits_scanned: 0` with empty arrays.
