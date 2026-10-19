---
version: 2
name: cross-cutting-web-test-files-use-factories-5d3e
description: "Adopt Test Data Factories in the Web Package: Web Test Files Use Factories"
metadata:
  actual:
    adr_id: 8bf33bc7-6ce8-513f-9b82-c5c9f9531f10
    status: accepted
    candidate_group_id: 8256f5b9-944d-5725-8ab2-ea52a9207523
    evidence:
      source: signal
  scope:
    root: packages/web
    paths:
      - "src/**/*.test.tsx"
    languages:
      - typescript
---

# Adopt Test Data Factories in the Web Package: Web Test Files Use Factories

Scope: applies to `packages/web/src/**/*.test.tsx`.

These rules are ALWAYS ACTIVE for the test files of the web package.

### Rules

- **R-WEBT-001** MUST: Web test files build their data with the shared factories rather than inline literals.

### Verify

```bash
grep -r "factory" packages/web/src/
```

**Accept when:**
- Web test files build their data with the shared factories rather than inline literals

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
