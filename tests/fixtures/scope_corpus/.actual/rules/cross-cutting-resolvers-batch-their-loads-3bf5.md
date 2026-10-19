---
version: 2
name: cross-cutting-resolvers-batch-their-loads-3bf5
description: "Adopt Additive Schema Evolution for the GraphQL API: Resolvers Batch Their Loads"
metadata:
  actual:
    adr_id: 2fa73415-bdbb-5e04-9a4e-42b7b545b2b9
    status: accepted
    candidate_group_id: 8a90c34f-1203-5853-9f4d-3d4682008832
    evidence:
      source: deep
  scope:
    root: .
    paths:
      - "services/api/graphql/**"
      - "services/api/graphql/resolvers/**"
      - "!services/api/graphql/**/*.generated.ts"
    languages:
      - typescript
---

# Adopt Additive Schema Evolution for the GraphQL API: Resolvers Batch Their Loads

Scope: applies to `services/api/graphql/**`, `services/api/graphql/resolvers/**`; excludes `!services/api/graphql/**/*.generated.ts`.

These rules are ALWAYS ACTIVE for all GraphQL schema definitions and resolvers in services/api/graphql/, covering field deprecation, nullability changes, and the compatibility checks run before release.

### Rules

- **R-GRAPHQ-011** MUST: List resolvers load children through a batching loader rather than per item.
- **R-GRAPHQ-012** MUST: Loaders are created per request so their cache cannot outlive it.
- **R-GRAPHQ-013** SHOULD: A resolver never issues an unbounded query.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "resolvers" services/api/graphql/ --include="*.ts"
grep -rE "resolvers|batch" services/api/graphql/resolvers/
test -d services/api/graphql/ && echo "governed tree present"
```

**Accept when:**
- List resolvers load children through a batching loader rather than per item
- Loaders are created per request so their cache cannot outlive it
- A resolver never issues an unbounded query

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
