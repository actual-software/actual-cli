---
version: 2
name: cross-cutting-deprecations-carry-a-reason-8bd0
description: "Adopt Additive Schema Evolution for the GraphQL API: Deprecations Carry A Reason"
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

# Adopt Additive Schema Evolution for the GraphQL API: Deprecations Carry A Reason

Scope: applies to `services/api/graphql/**`, `services/api/graphql/resolvers/**`; excludes `!services/api/graphql/**/*.generated.ts`.

These rules are ALWAYS ACTIVE for all GraphQL schema definitions and resolvers in services/api/graphql/, covering field deprecation, nullability changes, and the compatibility checks run before release.

### Rules

- **R-GRAPHQ-021** MUST: Every deprecated field states the replacement and the removal window.
- **R-GRAPHQ-022** MUST: Deprecated field usage is counted in metrics.
- **R-GRAPHQ-023** SHOULD: Removal happens only after usage reaches zero.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "deprecations" services/api/graphql/ --include="*.ts"
grep -rE "deprecations|carry" services/api/graphql/resolvers/
test -d services/api/graphql/ && echo "governed tree present"
```

**Accept when:**
- Every deprecated field states the replacement and the removal window
- Deprecated field usage is counted in metrics
- Removal happens only after usage reaches zero

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
