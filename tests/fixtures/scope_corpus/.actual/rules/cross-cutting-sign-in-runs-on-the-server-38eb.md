# Adopt Server-Side Session Handling for Next.js Authentication: Sign In Runs On The Server

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
