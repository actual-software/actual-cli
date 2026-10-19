---
version: 2
name: cross-cutting-server-client-is-used-server-side-320a
description: "Adopt Server Client Boundaries for Database Access: Server Client Is Used Server Side"
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

# Adopt Server Client Boundaries for Database Access: Server Client Is Used Server Side

Scope: applies to `backend/db/**`, `backend/db/policies/**`.

These rules are ALWAYS ACTIVE for all database interactions through the Supabase client in backend/db/, including row level security policy assumptions and the separation of browser and server clients.

### Rules

- **R-SUPABA-001** MUST: Server-side code constructs the client from the service role key held in the environment.
- **R-SUPABA-002** MUST: The browser client is never imported by server modules.
- **R-SUPABA-003** SHOULD: Client construction happens per request, never at module scope.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "server" backend/db/ --include="*.py"
grep -rE "server|client" backend/db/policies/
test -d backend/db/ && echo "governed tree present"
```

**Accept when:**
- Server-side code constructs the client from the service role key held in the environment
- The browser client is never imported by server modules
- Client construction happens per request, never at module scope

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
