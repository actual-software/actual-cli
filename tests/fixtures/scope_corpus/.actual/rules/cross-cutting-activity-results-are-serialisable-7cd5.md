---
version: 2
name: cross-cutting-activity-results-are-serialisable-7cd5
description: "Adopt Typed Input Models for Temporal Activity Functions: Activity Results Are Serialisable"
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

# Adopt Typed Input Models for Temporal Activity Functions: Activity Results Are Serialisable

Scope: applies to `backend/workers/activities/**`, `backend/workers/workflows/**`.

These rules are ALWAYS ACTIVE for all Temporal activity functions in backend/workers/activities/, their input and output models, and the workflow definitions that invoke them.

### Rules

- **R-TEMPOR-031** MUST: Activity return values are Pydantic models or primitives, never ORM instances.
- **R-TEMPOR-032** MUST: Large payloads are written to object storage and referenced by key.
- **R-TEMPOR-033** SHOULD: Result models carry an explicit schema version field.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "activity" backend/workers/activities/ --include="*.py"
grep -rE "activity|results" backend/workers/workflows/
test -d backend/workers/activities/ && echo "governed tree present"
```

**Accept when:**
- Activity return values are Pydantic models or primitives, never ORM instances
- Large payloads are written to object storage and referenced by key
- Result models carry an explicit schema version field

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
