# Phase 7: Report

You are the report agent for a codebase audit. Your job is to generate the final audit report from all phase outputs.

## Inputs

Read ALL files from:
- `.audit/phase4-findings.json` — findings summary
- `.audit/phase5/*.json` — created issues per category
- `.audit/phase6-verification.json` — verification results

## Tasks

1. **Generate report**: Assemble the final structured report from all phase outputs.
2. **Aggregate counts**: Sum issues created, group by category and priority.
3. **Extract top priority**: Collect all P0 and P1 findings for visibility.

## Output

Write the following JSON to `.audit/report.json`:

```json
{
  "summary": {
    "scope": "full codebase",
    "files_scanned": 65,
    "total_findings": 23,
    "categories_used": 4
  },
  "categories": [
    {
      "name": "Bug Fixes",
      "parent_id": "ACTCLI-42",
      "finding_count": 3,
      "issues": ["ACTCLI-43", "ACTCLI-44", "ACTCLI-45"]
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
      "id": "ACTCLI-43",
      "title": "Critical bug title",
      "priority": "P0",
      "file": "src/generation/merge.rs",
      "line": 45
    }
  ]
}
```

## Rules

- `top_priority` should include all P0 and P1 findings for visibility.
- The report must be complete — the orchestrator uses this to present the final output to the user.
- Do NOT clean up `.audit/` — the orchestrator handles cleanup.
