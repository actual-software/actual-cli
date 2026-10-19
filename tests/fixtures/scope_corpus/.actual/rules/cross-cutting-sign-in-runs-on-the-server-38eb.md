---
version: 2
name: cross-cutting-sign-in-runs-on-the-server-38eb
description: "Adopt Server-Side Session Handling for Next.js Authentication: Sign In Runs On The Server"
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

# Adopt Server-Side Session Handling for Next.js Authentication: Sign In Runs On The Server

Scope: applies to `web/lib/auth.ts`, `web/app/**`.

These rules are ALWAYS ACTIVE for all server-side authentication operations in the Next.js application, including user sign-in, registration, session refresh, and demo user authentication flows.

### Rules

- **R-NEXTAU-001** MUST: Credential sign-in is performed in a server action, never in a client component.
- **R-NEXTAU-002** MUST: The session cookie is set with HttpOnly, Secure and SameSite=Lax.
- **R-NEXTAU-003** SHOULD: Sign-in failures return a generic message and log the specific cause server-side.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "sign" web/lib/auth.ts --include="*.ts"
grep -rE "sign|runs" web/app/
test -d web/lib/auth.ts && echo "governed tree present"
```

**Accept when:**
- Credential sign-in is performed in a server action, never in a client component
- The session cookie is set with HttpOnly, Secure and SameSite=Lax
- Sign-in failures return a generic message and log the specific cause server-side

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
