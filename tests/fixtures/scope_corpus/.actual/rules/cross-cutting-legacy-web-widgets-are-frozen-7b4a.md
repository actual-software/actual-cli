---
version: 2
name: cross-cutting-legacy-web-widgets-are-frozen-7b4a
description: "Adopt a Freeze on Legacy Web Widgets: Legacy Web Widgets Are Frozen"
metadata:
  actual:
    adr_id: a3a66479-668c-5420-8962-c7ad2563bf1c
    status: superseded
    candidate_group_id: d174129e-c91a-5a8a-976b-24aa0957987e
    evidence:
      source: signal
  scope:
    root: packages/web
    paths:
      - "src/components/**"
    languages:
      - typescript
---

# Adopt a Freeze on Legacy Web Widgets: Legacy Web Widgets Are Frozen

Scope: applies to `packages/web/src/components/**`.

These rules were ALWAYS ACTIVE for the legacy widgets of the web package before the freeze was lifted.

### Rules

- **R-LEGW-001** MUST NOT: Legacy web widgets gain new props or new dependencies.

### Verify

```bash
grep -r "widget" packages/web/src/components/
```

**Accept when:**
- Legacy web widgets gain new props or new dependencies

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
