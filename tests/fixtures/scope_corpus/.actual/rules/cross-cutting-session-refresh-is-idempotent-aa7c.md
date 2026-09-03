# Adopt Server-Side Session Handling for Next.js Authentication: Session Refresh Is Idempotent

These rules are ALWAYS ACTIVE for all server-side authentication operations in the Next.js application, including user sign-in, registration, session refresh, and demo user authentication flows.

### Rules

- **R-NEXTAU-011** MUST: Concurrent refresh attempts for one session converge on a single new session.
- **R-NEXTAU-012** MUST: A refresh that races a sign-out leaves the session revoked.
- **R-NEXTAU-013** SHOULD: Refresh writes go through the server client, never the browser client.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "session" web/lib/auth.ts --include="*.ts"
grep -rE "session|refresh" web/app/
test -d web/lib/auth.ts && echo "governed tree present"
```

**Accept when:**
- Concurrent refresh attempts for one session converge on a single new session
- A refresh that races a sign-out leaves the session revoked
- Refresh writes go through the server client, never the browser client

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
