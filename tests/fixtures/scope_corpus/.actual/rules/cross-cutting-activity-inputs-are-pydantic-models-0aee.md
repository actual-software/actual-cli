---
version: 2
name: cross-cutting-activity-inputs-are-pydantic-models-0aee
description: "Adopt Typed Input Models for Temporal Activity Functions: Activity Inputs Are Pydantic Models"
metadata:
  actual:
    adr_id: 55a5eaec-ece9-5196-adb1-98eed257d58a
    status: updated
    candidate_group_id: 41b74c55-14c6-5daf-ab40-7ba88f54cac6
    evidence:
      source: signal
  scope:
    root: .
    paths:
      - "backend/workers/activities/**"
      - "backend/workers/workflows/**"
    languages:
      - python
---

# Adopt Typed Input Models for Temporal Activity Functions: Activity Inputs Are Pydantic Models

Scope: applies to `backend/workers/activities/**`, `backend/workers/workflows/**`.

These rules are ALWAYS ACTIVE for all Temporal activity functions in backend/workers/activities/, their input and output models, and the workflow definitions that invoke them.

### Rules

- **R-TEMPOR-001** MUST: Every activity function accepts exactly one Pydantic model as its input argument.
- **R-TEMPOR-002** MUST: Input models forbid extra fields so an unknown key fails fast rather than being silently dropped.
- **R-TEMPOR-003** SHOULD: Field validators normalise identifiers before the activity body runs.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "activity" backend/workers/activities/ --include="*.py"
grep -rE "activity|inputs" backend/workers/workflows/
test -d backend/workers/activities/ && echo "governed tree present"
```

**Accept when:**
- Every activity function accepts exactly one Pydantic model as its input argument
- Input models forbid extra fields so an unknown key fails fast rather than being silently dropped
- Field validators normalise identifiers before the activity body runs

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
