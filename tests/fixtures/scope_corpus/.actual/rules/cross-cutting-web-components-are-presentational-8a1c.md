---
version: 2
name: cross-cutting-web-components-are-presentational-8a1c
description: "Adopt Presentational Components in the Web Package: Web Components Are Presentational"
metadata:
  actual:
    adr_id: 1da9ed33-60ac-5835-b493-6a4d20eb5ad9
    status: accepted
    candidate_group_id: 5622a1c7-18f9-5b26-a6a6-c118b2c37eec
    evidence:
      source: deep
  scope:
    root: packages/web
    paths:
      - "src/components/**"
      - "!src/**/*.test.tsx"
    languages:
      - typescript
---

# Adopt Presentational Components in the Web Package: Web Components Are Presentational

Scope: applies to `packages/web/src/components/**`; excludes `!packages/web/src/**/*.test.tsx`.

These rules are ALWAYS ACTIVE for the presentational components of the web package.

### Rules

- **R-WEBC-001** MUST: Web components receive data and callbacks through props and hold no data-fetching logic.
- **R-WEBC-002** MUST NOT: Web components import from the api package.

### Verify

```bash
grep -r "fetch" packages/web/src/components/
```

**Accept when:**
- Web components receive data and callbacks through props and hold no data-fetching logic
- Web components import from the api package

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
