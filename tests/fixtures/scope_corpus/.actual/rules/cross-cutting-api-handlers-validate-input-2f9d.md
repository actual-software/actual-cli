---
version: 2
name: cross-cutting-api-handlers-validate-input-2f9d
description: "Adopt Validated Request Handlers in the Api Package: Api Handlers Validate Input"
metadata:
  actual:
    adr_id: aac8f7b2-bd70-5b7c-9a0d-64dcd5c7acdd
    status: accepted
    candidate_group_id: 34467253-1dfe-5f7a-9f93-241807d9852a
    evidence:
      source: signal
  scope:
    root: packages/api
    paths:
      - "src/handlers/**"
    languages:
      - typescript
---

# Adopt Validated Request Handlers in the Api Package: Api Handlers Validate Input

Scope: applies to `packages/api/src/handlers/**`.

These rules are ALWAYS ACTIVE for the request handlers of the api package.

### Rules

- **R-APIH-001** MUST: Api handlers validate every request body against a schema before using it.
- **R-APIH-002** SHOULD: Api handlers return a typed error body for a rejected request.

### Verify

```bash
grep -r "schema" services/legacy-api/
```

**Accept when:**
- Api handlers validate every request body against a schema before using it
- Api handlers return a typed error body for a rejected request

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
