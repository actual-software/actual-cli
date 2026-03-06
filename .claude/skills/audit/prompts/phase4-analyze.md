# Phase 4: Analysis & Merge

You are the analysis agent for a codebase audit. Your job is to merge findings from Phase 2 (source reading) and Phase 3 (automated scans), deduplicate, categorize, and produce a consolidated findings list organized by epic.

## Inputs

Read ALL files from:
- `.audit/prep.json` — existing parent issues and issues (to avoid duplicates)
- `.audit/phase2/*.json` — source-reading findings
- `.audit/phase3/*.json` — automated scan results

## Tasks

1. **Merge**: Combine all Phase 2 findings and Phase 3 scan matches into a single list.
2. **Deduplicate**: Remove findings that point to the same file+line or describe the same issue. Prefer the Phase 2 finding (richer context) over the Phase 3 match.
3. **Filter existing**: Remove any finding that already has an open issue (check `.audit/prep.json` existing_issues by matching file paths and descriptions).
4. **Categorize into epics**: Group findings by theme. Map each finding to an epic:
   - **Bugs/Correctness** → "Bug Fixes" epic
   - **Error Handling** → "Error Handling & Robustness Gaps" epic
   - **Test Quality** → "Test Quality & Coverage Gaps" epic
   - **Refactoring + DRY** → "Code Structure & Maintainability" epic
   - **Cut Corners + Missing Safeguards** → "Missing Safeguards & Input Validation" epic
   - **Half-Implemented + Dead Code** → "Dead Code & Incomplete Features" epic
5. **Assign priority**: Ensure each finding has a priority:
   - P0: Broken for users, data loss, security
   - P1: Wrong behavior, workaround exists
   - P2: Robustness gap, bad tests, high-value refactors
   - P3: Cleanup, dead code, maintainability
6. **Quality filter**: Drop any finding that is:
   - A style preference, not a real problem
   - Too vague to be actionable
    - Trivially obvious and not worth filing

## Output

Write the following JSON to `.audit/phase4-findings.json`:

```json
{
  "summary": {
    "total_findings": 23,
    "by_category": {"bugs": 3, "error_handling": 8, "test_quality": 5, "refactoring": 4, "cut_corners": 2, "dead_code": 1},
    "by_priority": {"P0": 1, "P1": 5, "P2": 12, "P3": 5},
    "duplicates_removed": 4,
    "existing_issues_filtered": 2
  },
  "epics": [
    {
      "name": "Bug Fixes",
      "existing_epic_id": "actual-cli-drj",
      "needs_creation": false,
      "priority": "P0",
      "findings": [
        {
          "title": "Concise, specific title",
          "file": "src/generation/merge.rs",
          "line": 45,
          "category": "bugs",
          "severity": "P0",
          "description": "What the code does now vs what it should do.",
          "impact": "Why this matters. What breaks.",
          "suggested_fix": "Concrete implementation approach. Name functions to change.",
          "testing": "What tests to add or modify.",
          "code_snippet": "the problematic code"
        }
      ]
    },
    {
      "name": "Error Handling & Robustness Gaps",
      "existing_epic_id": null,
      "needs_creation": true,
      "priority": "P2",
      "findings": []
    }
  ]
}
```

## Rules

- Every finding must have all 5 description fields (title, description, impact, suggested_fix, testing).
- A cold-start agent must be able to pick up any finding and implement the fix without additional research.
- Do NOT create issues — just produce the consolidated analysis. Phase 5 handles issue creation.
- If an epic has 0 findings after dedup/filtering, include it with an empty findings array so the orchestrator knows.
- Findings should be ordered by priority within each epic (P0 first).
