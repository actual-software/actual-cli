---
version: 2
name: cross-cutting-row-level-security-is-assumed-on-3d5e
description: "Adopt Server Client Boundaries for Database Access: Row Level Security Is Assumed On"
metadata:
  actual:
    adr_id: dece1d7d-4db3-52ce-9a19-8595b5c1a580
    status: created-from-user
    candidate_group_id: a52b755f-3f37-5d97-9b0f-49b3a69f8917
    evidence:
      source: signal
  scope:
    root: backend/db
    paths:
      - "**"
      - "policies/**"
    languages:
      - python
---

# Adopt Server Client Boundaries for Database Access: Row Level Security Is Assumed On

Scope: applies to `backend/db/**`, `backend/db/policies/**`.

These rules are ALWAYS ACTIVE for all database interactions through the Supabase client in backend/db/, including row level security policy assumptions and the separation of browser and server clients.

### Rules

- **R-SUPABA-011** MUST: Every table read assumes row level security is enabled and passes the caller identity.
- **R-SUPABA-012** MUST: A query that must bypass policy is isolated in a named function and commented.
- **R-SUPABA-013** SHOULD: Policy changes ship with a test that asserts the denied case.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "level" backend/db/ --include="*.py"
grep -rE "level|security" backend/db/policies/
test -d backend/db/ && echo "governed tree present"
```

**Accept when:**
- Every table read assumes row level security is enabled and passes the caller identity
- A query that must bypass policy is isolated in a named function and commented
- Policy changes ship with a test that asserts the denied case

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
