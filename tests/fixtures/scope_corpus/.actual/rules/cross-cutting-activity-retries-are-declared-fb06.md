# Adopt Typed Input Models for Temporal Activity Functions: Activity Retries Are Declared

These rules are ALWAYS ACTIVE for all Temporal activity functions in backend/workers/activities/, their input and output models, and the workflow definitions that invoke them.

### Rules

- **R-TEMPOR-011** MUST: Every activity declares an explicit retry policy rather than inheriting the worker default.
- **R-TEMPOR-012** MUST: Non-retryable error types are enumerated on the retry policy.
- **R-TEMPOR-013** SHOULD: Activity timeouts are set to at least twice the observed p99 duration.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "activity" backend/workers/activities/ --include="*.py"
grep -rE "activity|retries" backend/workers/workflows/
test -d backend/workers/activities/ && echo "governed tree present"
```

**Accept when:**
- Every activity declares an explicit retry policy rather than inheriting the worker default
- Non-retryable error types are enumerated on the retry policy
- Activity timeouts are set to at least twice the observed p99 duration

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
