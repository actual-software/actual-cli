# Adopt Server Client Boundaries for Database Access: Server Client Is Used Server Side

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
