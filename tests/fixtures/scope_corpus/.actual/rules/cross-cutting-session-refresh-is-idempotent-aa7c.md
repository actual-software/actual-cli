---
version: 2
name: cross-cutting-session-refresh-is-idempotent-aa7c
description: "Adopt Server-Side Session Handling for Next.js Authentication: Session Refresh Is Idempotent"
metadata:
  actual:
    adr_id: 1eca5e80-f15f-5533-bf19-40d04cc04bbb
    status: accepted
    candidate_group_id: bb49cc65-8800-584b-9730-d793a599ad49
    evidence:
      source: deep
  scope:
    root: web
    paths:
      - lib/auth.ts
      - "app/**"
    languages:
      - typescript
---

# Adopt Server-Side Session Handling for Next.js Authentication: Session Refresh Is Idempotent

Scope: applies to `web/lib/auth.ts`, `web/app/**`.

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
