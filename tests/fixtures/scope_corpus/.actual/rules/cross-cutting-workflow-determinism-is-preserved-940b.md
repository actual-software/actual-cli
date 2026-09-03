# Adopt Typed Input Models for Temporal Activity Functions: Workflow Determinism Is Preserved

These rules are ALWAYS ACTIVE for all Temporal activity functions in backend/workers/activities/, their input and output models, and the workflow definitions that invoke them.

### Rules

- **R-TEMPOR-021** MUST: Workflow code performs no direct I/O, network access, or clock reads.
- **R-TEMPOR-022** MUST: Random values used inside a workflow come from the deterministic workflow random source.
- **R-TEMPOR-023** SHOULD: Changes to workflow logic ship behind a versioning marker.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "workflow" backend/workers/activities/ --include="*.py"
grep -rE "workflow|determinism" backend/workers/workflows/
test -d backend/workers/activities/ && echo "governed tree present"
```

**Accept when:**
- Workflow code performs no direct I/O, network access, or clock reads
- Random values used inside a workflow come from the deterministic workflow random source
- Changes to workflow logic ship behind a versioning marker

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
