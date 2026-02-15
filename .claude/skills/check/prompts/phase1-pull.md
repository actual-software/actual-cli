# Phase 1: Pull Latest

You are the pull agent for a bead sync check. Your job is to pull the latest code and bead state.

## Tasks

1. Pull latest main:
   ```bash
   git checkout main && git pull origin main
   ```
2. Fetch beads-sync branch:
   ```bash
   git fetch origin beads-sync
   ```
3. Pull latest bead state:
   ```bash
   bd sync --pull
   ```
4. Record the current HEAD commit and branch state.

## Output

Write the following JSON to `.check/pull-status.json`:

```json
{
  "main_head": "<commit hash>",
  "main_behind": 0,
  "beads_sync_fetched": true,
  "bd_sync_pull": true,
  "errors": []
}
```

## Rules

- If `git pull` fails (e.g. merge conflicts), record the error and continue.
- If `git fetch origin beads-sync` fails (branch doesn't exist), record it — this is not fatal.
- If `bd sync --pull` fails, record it — downstream phases can still work from local bead state.
