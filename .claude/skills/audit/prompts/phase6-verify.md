# Phase 6: Verification

You are the verification agent for a codebase audit. Your job is to verify that all beads were created correctly, organized under epics, and no duplicates exist.

## Inputs

Read ALL files from:
- `.audit/phase5/*.json` — bead creation results per epic
- `.audit/phase4-findings.json` — expected findings (for count comparison)

## Tasks

1. **Count check**: Compare total beads created (sum across all phase5 files) against total findings in phase4. Report any discrepancy.
2. **Epic organization**: Run `bd list | grep epic` and verify every created bead has a parent epic.
3. **Duplicate check**: Run `bd list` and look for beads with identical or very similar titles.
4. **Description spot-check**: Run `bd show <id>` on 3-5 random beads and verify they have all 5 description sections (Location, Observed vs Expected, Impact, Fix, Testing).
5. **Error review**: Check all phase5 files for any entries in their `errors` arrays. Report them.

## Output

Write the following JSON to `.audit/phase6-verification.json`:

```json
{
  "passed": true,
  "checks": {
    "count_match": {
      "passed": true,
      "expected": 23,
      "actual": 23
    },
    "epic_organization": {
      "passed": true,
      "orphaned_beads": []
    },
    "duplicates": {
      "passed": true,
      "potential_duplicates": []
    },
    "description_quality": {
      "passed": true,
      "sampled": ["actual-cli-abc", "actual-cli-def", "actual-cli-ghi"],
      "missing_sections": []
    },
    "errors": {
      "passed": true,
      "error_count": 0,
      "details": []
    }
  }
}
```

## Rules

- Set top-level `passed` to false if ANY check fails.
- For duplicate detection, flag beads with >80% title similarity or same file+line.
- For description quality, check for the literal section headers: "Location", "Observed", "Expected", "Impact", "Fix", "Testing".
- Report ALL issues — don't stop at the first failure.
- Do NOT fix issues — just report them. The orchestrator will decide what to do.
