# Adopt Typed Input Models for Temporal Activity Functions: Activity Results Are Serialisable

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
