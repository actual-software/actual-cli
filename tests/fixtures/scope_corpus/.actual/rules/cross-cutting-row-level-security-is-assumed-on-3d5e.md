# Adopt Server Client Boundaries for Database Access: Row Level Security Is Assumed On

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
