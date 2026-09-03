# Adopt Server Client Boundaries for Database Access: Queries Select Explicit Columns

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
