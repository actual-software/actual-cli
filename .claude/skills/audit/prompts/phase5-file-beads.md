# Phase 5: File Beads

You are a bead-filing agent for a codebase audit. Your job is to create beads in the issue tracker for a specific epic's findings.

## Inputs

- `EPIC_KEY`: {{EPIC_KEY}} (the key in the epics array, e.g. "Bug Fixes")
- Read `.audit/phase4-findings.json` — find the epic matching your EPIC_KEY
- Read `.audit/prep.json` — for existing epic IDs

## Tasks

1. Read the findings for your assigned epic from `.audit/phase4-findings.json`.
2. If the epic already exists (`existing_epic_id` is set), use that ID. Otherwise create it:
   ```bash
   bd create "<Epic Name>" -p <priority> --type epic --description "<theme description>"
   ```
3. For each finding in the epic, create a bead with a full 5-section description:
   ```bash
   bd create "<title>" -p <priority> --parent <epic-id> --description "<description>"
   ```

### Bead Description Format

Every bead description MUST contain all 5 sections:

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
  "epic_id": "actual-cli-drj",
  "epic_created": false,
  "beads_created": [
    {
      "id": "actual-cli-abc",
      "title": "Race condition in merge processor",
      "priority": "P1",
      "file": "src/generation/merge.rs",
      "line": 42
    }
  ],
  "beads_skipped": [],
  "errors": []
}
```

## Rules

- If there are 0 findings for this epic, write an empty `beads_created` array and exit.
- Do NOT create duplicate beads — check existing beads from `.audit/prep.json`.
- If `bd create` fails for any bead, record it in `errors` and continue with the next.
- Use the exact title and priority from the findings — do not change them.
- Keep descriptions concise but complete — all 5 sections required.
- Do NOT run `bd sync` — that's Phase 7's job.
