# Phase 5: File Linear Issues

You are an issue-filing agent for a codebase audit. Your job is to create Linear issues in the `actcli` project for a specific category's findings.

## Inputs

- `EPIC_KEY`: {{EPIC_KEY}} (the key in the epics array, e.g. "Bug Fixes")
- Read `.audit/phase4-findings.json` — find the epic matching your EPIC_KEY
- Read `.audit/prep.json` — for existing parent issue IDs

## Tasks

1. Read the findings for your assigned category from `.audit/phase4-findings.json`.
2. If the parent issue already exists (`existing_parent_id` is set), use that ID. Otherwise create it using the Linear API via `gh api`:
   ```bash
   # Create a parent issue in the actcli project
   gh api graphql -f query='
     mutation {
       issueCreate(input: {
         teamId: "<team-id>"
         title: "<Category Name>"
         description: "<theme description>"
         priority: <priority-number>
       }) {
         issue { id identifier title }
       }
     }
   '
   ```
3. For each finding in the category, create a sub-issue with a full 5-section description:
   ```bash
   gh api graphql -f query='
     mutation {
       issueCreate(input: {
         teamId: "<team-id>"
         title: "<finding title>"
         description: "<description>"
         priority: <priority-number>
         parentId: "<parent-issue-id>"
       }) {
         issue { id identifier title }
       }
     }
   '
   ```

### Issue Description Format

Every issue description MUST contain all 5 sections:

```
**Location:** <file>:<line>

**Observed vs Expected:**
Observed: <what the code does now>
Expected: <what it should do>

**Impact:** <why this matters, what breaks>

**Fix:** <concrete implementation approach — name files, functions, patterns>

**Testing:** <what tests to add/modify, assertion strategy>
```

## Output

Write the following JSON to `.audit/phase5/{{EPIC_SLUG}}.json`:

```json
{
  "epic_key": "Bug Fixes",
  "parent_id": "ACTCLI-42",
  "parent_created": false,
  "issues_created": [
    {
      "id": "ACTCLI-43",
      "title": "Race condition in merge processor",
      "priority": "P1",
      "file": "src/generation/merge.rs",
      "line": 42
    }
  ],
  "issues_skipped": [],
  "errors": []
}
```

## Rules

- If there are 0 findings for this category, write an empty `issues_created` array and exit.
- Do NOT create duplicate issues — check existing issues from `.audit/prep.json`.
- If issue creation fails, record it in `errors` and continue with the next.
- Use the exact title and priority from the findings — do not change them.
- Keep descriptions concise but complete — all 5 sections required.
