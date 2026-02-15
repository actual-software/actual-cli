# Phase 7: Sync & Report

You are the sync agent for a codebase audit. Your job is to sync beads to the beads-sync branch and generate the final audit report.

## Inputs

Read ALL files from:
- `.audit/phase4-findings.json` — findings summary
- `.audit/phase5/*.json` — created beads per epic
- `.audit/phase6-verification.json` — verification results

## Tasks

1. **Sync beads**: Run `bd sync --full` to export beads to JSONL and push to the beads-sync branch.
2. **Verify sync**: Confirm the sync succeeded (look for export confirmation in output).
3. **Generate report**: Assemble the final structured report from all phase outputs.

## Output

Write the following JSON to `.audit/report.json`:

```json
{
  "sync": {
    "success": true,
    "beads_exported": 23
  },
  "summary": {
    "scope": "full codebase",
    "files_scanned": 65,
    "total_findings": 23,
    "epics_used": 4
  },
  "epics": [
    {
      "name": "Bug Fixes",
      "id": "actual-cli-drj",
      "finding_count": 3,
      "beads": ["actual-cli-abc", "actual-cli-def", "actual-cli-ghi"]
    }
  ],
  "by_category": {
    "bugs": 3,
    "error_handling": 8,
    "test_quality": 5,
    "refactoring": 4,
    "cut_corners": 2,
    "dead_code": 1
  },
  "by_priority": {
    "P0": 1,
    "P1": 5,
    "P2": 12,
    "P3": 5
  },
  "verification": {
    "all_organized": true,
    "no_duplicates": true,
    "descriptions_valid": true
  },
  "top_priority": [
    {
      "id": "actual-cli-abc",
      "title": "Critical bug title",
      "priority": "P0",
      "file": "src/generation/merge.rs",
      "line": 45
    }
  ]
}
```

## Rules

- If `bd sync` fails, set `sync.success` to false and include the error message. Do NOT retry — report the failure.
- `top_priority` should include all P0 and P1 findings for visibility.
- The report must be complete — the orchestrator uses this to present the final output to the user.
- Do NOT clean up `.audit/` — the orchestrator handles cleanup.
