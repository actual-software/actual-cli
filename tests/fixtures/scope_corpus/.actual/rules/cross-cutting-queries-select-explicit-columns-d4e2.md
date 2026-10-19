---
version: 2
name: cross-cutting-queries-select-explicit-columns-d4e2
description: "Adopt Server Client Boundaries for Database Access: Queries Select Explicit Columns"
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

# Adopt Server Client Boundaries for Database Access: Queries Select Explicit Columns

Scope: applies to `backend/db/**`, `backend/db/policies/**`.

These rules are ALWAYS ACTIVE for all database interactions through the Supabase client in backend/db/, including row level security policy assumptions and the separation of browser and server clients.

### Rules

- **R-SUPABA-021** MUST: Queries name the columns they need rather than selecting everything.
- **R-SUPABA-022** MUST: Joins declare the foreign key relationship explicitly.
- **R-SUPABA-023** SHOULD: Pagination uses keyset ranges rather than offsets.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "queries" backend/db/ --include="*.py"
grep -rE "queries|select" backend/db/policies/
test -d backend/db/ && echo "governed tree present"
```

**Accept when:**
- Queries name the columns they need rather than selecting everything
- Joins declare the foreign key relationship explicitly
- Pagination uses keyset ranges rather than offsets

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
